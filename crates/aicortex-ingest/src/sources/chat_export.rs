//! Chat exports (spec 031 B-2, B-4, B-9, B-10).
//!
//! One adapter, with a parser per export shape, each recognised by what a
//! conversation holds and never by a file name. A conversation becomes one
//! memory per message: each turn keeps its own author, so an assistant turn is
//! an assertion by an agent and a user turn is the user's, and each can be
//! redacted or forgotten alone. There is no memory for the thread.
//!
//! The two shapes understood today:
//!
//! - `tree`: a conversation holds a `mapping` of nodes, each with a `parent`,
//!   `children`, and a `message` whose `author.role`, `create_time` and
//!   `content.parts` say who spoke, when, and what. The branch kept is the
//!   last child at every fork (D-4).
//! - `flat`: a conversation holds a `messages` array whose entries have a
//!   `sender` or `role`, a `text`, and a `created_at`.
//!
//! A file is an array of conversations, or an object whose `conversations`
//! is one.

use std::collections::BTreeSet;
use std::sync::Arc;

use aicortex_types::{ConversationId, ConversationRef, MemoryKind};
use rahi_types::UnixSeconds;
use serde_json::{Value, json};

use super::{Loaded, derived_conversation_id, page_of, time_of};
use crate::adapter::{
    AdapterContext, AdapterError, AdapterResult, Author, BoxFuture, Candidate, Page, Probe,
    RawItem, SourceAdapter, SourceId, SourceKind,
};
use crate::cursor::Cursor;
use crate::upload::Staging;

/// The name the adapter registers under.
pub const SOURCE: &str = "chat-export";

/// The most messages of one conversation read; a longer thread is cut and its
/// remainder is not imported (D-5).
const MAX_MESSAGES: usize = 20_000;

/// One message as a parser read it.
#[derive(Debug)]
struct Message {
    id: Option<String>,
    role: String,
    name: Option<String>,
    text: Option<String>,
    at: Option<u64>,
}

/// One conversation as a parser read it.
#[derive(Debug)]
struct Conversation {
    parser: &'static str,
    id: Option<String>,
    created: Option<u64>,
    messages: Vec<Message>,
}

/// The chat export adapter.
#[derive(Debug)]
pub struct ChatExport {
    staging: Arc<Staging>,
    loaded: Loaded,
}

impl ChatExport {
    /// An adapter reading uploads from `staging`.
    #[must_use]
    pub fn new(staging: Arc<Staging>) -> Self {
        Self {
            staging,
            loaded: Loaded::default(),
        }
    }

    fn items(&self, ctx: &AdapterContext) -> AdapterResult<Arc<Vec<RawItem>>> {
        self.loaded.items(&self.staging, ctx, items_of)
    }
}

impl SourceAdapter for ChatExport {
    fn id(&self) -> SourceId {
        SourceId::new(SOURCE).unwrap_or_else(|_| unreachable!("a constant valid name"))
    }

    fn kind(&self) -> SourceKind {
        SourceKind::Export
    }

    fn probe<'a>(&'a self, ctx: &'a AdapterContext) -> BoxFuture<'a, AdapterResult<Probe>> {
        Box::pin(async move {
            let items = self.items(ctx)?;
            Ok(Probe {
                label: "chat export".to_owned(),
                estimated_items: Some(items.len() as u64),
            })
        })
    }

    fn page<'a>(
        &'a self,
        ctx: &'a AdapterContext,
        cursor: Option<&'a Cursor>,
    ) -> BoxFuture<'a, AdapterResult<Page>> {
        Box::pin(async move { page_of(&self.items(ctx)?, ctx, cursor) })
    }

    fn finish<'a>(&'a self, ctx: &'a AdapterContext, completed: bool) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            if completed && let Ok((scope, id)) = super::upload_of(ctx) {
                self.staging.delete(&scope, &id);
            }
            self.loaded.clear();
        })
    }

    fn map(&self, item: &RawItem) -> AdapterResult<Candidate> {
        let payload = &item.payload;
        let unmapped = |why: &str| AdapterError::Fatal(why.to_owned());
        // A message with no text, and a conversation refused for its id, are
        // delivered so the run counts them, and are never stored (B-9).
        let text = payload
            .get("text")
            .and_then(Value::as_str)
            .ok_or_else(|| unmapped("an item with no text"))?;
        let id = payload
            .get("conversation")
            .and_then(Value::as_str)
            .and_then(|id| ConversationId::new(id).ok())
            .ok_or_else(|| unmapped("an item with no conversation"))?;
        let turn = payload
            .get("turn")
            .and_then(Value::as_u64)
            .and_then(|turn| u32::try_from(turn).ok())
            .ok_or_else(|| unmapped("an item with no turn"))?;
        let reference =
            ConversationRef::new(id, turn).map_err(|error| unmapped(&error.to_string()))?;
        let name = payload.get("name").and_then(Value::as_str);
        let author = match payload.get("author").and_then(Value::as_str) {
            Some("user") => Author::User,
            Some("assistant") => Author::Assistant,
            Some(other) => Author::Participant(name.unwrap_or(other).to_owned()),
            None => Author::Participant(name.unwrap_or("participant").to_owned()),
        };
        Ok(Candidate::new(MemoryKind::Observation, text)
            .by(author)
            .in_conversation(reference))
    }
}

/// Every message of every conversation as an item, in export order.
fn items_of(bytes: &[u8]) -> AdapterResult<Vec<RawItem>> {
    let root: Value = serde_json::from_slice(bytes)
        .map_err(|_| AdapterError::Fatal("the upload is not a JSON export".to_owned()))?;
    let list = match &root {
        Value::Array(list) => list,
        Value::Object(map) => map
            .get("conversations")
            .and_then(Value::as_array)
            .ok_or_else(|| AdapterError::Fatal("the upload is not a chat export".to_owned()))?,
        _ => {
            return Err(AdapterError::Fatal(
                "the upload is not a chat export".to_owned(),
            ));
        }
    };
    let mut items = Vec::new();
    let mut seen = BTreeSet::new();
    for (index, raw) in list.iter().enumerate() {
        let Some(conversation) = parse(raw) else {
            items.push(refusal(index, "unrecognised"));
            continue;
        };
        let Some((first_text, first_at)) = conversation
            .messages
            .iter()
            .find_map(|m| m.text.as_deref().map(|text| (text, m.at)))
        else {
            // No message has text: nothing is stored and no id is needed.
            continue;
        };
        let id = match &conversation.id {
            Some(id) => id.clone(),
            None => {
                let created = conversation.created.or(first_at).unwrap_or(0);
                derived_conversation_id(conversation.parser, UnixSeconds::new(created), first_text)
            }
        };
        if ConversationId::new(&id).is_err() || !seen.insert(id.clone()) {
            // An id that cannot be a key, or that an earlier conversation of
            // this export holds: refused, never merged (B-9).
            items.push(refusal(index, "id"));
            continue;
        }
        let mut last = conversation.created.or(first_at).unwrap_or(0);
        for (position, message) in conversation.messages.iter().enumerate() {
            let turn = position + 1;
            let external = message
                .id
                .clone()
                .filter(|id| !id.is_empty())
                .unwrap_or_else(|| format!("{id}/{turn}"));
            last = message.at.unwrap_or(last);
            items.push(RawItem::new(
                external,
                UnixSeconds::new(last),
                json!({
                    "conversation": id,
                    "turn": turn,
                    "author": message.role,
                    "name": message.name,
                    "text": message.text,
                }),
            ));
        }
    }
    Ok(items)
}

fn refusal(index: usize, why: &str) -> RawItem {
    RawItem::new(
        format!("refused:{index}:{why}"),
        UnixSeconds::new(0),
        json!({ "refused": why }),
    )
}

/// The parser that recognises `raw`, by content.
fn parse(raw: &Value) -> Option<Conversation> {
    if raw.get("mapping").is_some_and(Value::is_object) {
        Some(tree(raw))
    } else if raw.get("messages").is_some_and(Value::is_array) {
        Some(flat(raw))
    } else {
        None
    }
}

fn string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::to_owned)
        .filter(|text| !text.trim().is_empty())
}

fn role(value: &str) -> String {
    match value.to_ascii_lowercase().as_str() {
        "user" | "human" => "user".to_owned(),
        "assistant" | "model" | "ai" | "bot" => "assistant".to_owned(),
        other => other.to_owned(),
    }
}

fn tree(raw: &Value) -> Conversation {
    let empty = serde_json::Map::new();
    let nodes = raw
        .get("mapping")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let mut messages = Vec::new();
    let mut visited = BTreeSet::new();
    // The first root in key order, then the last child at every fork.
    let mut at = nodes
        .iter()
        .find(|(_, node)| {
            node.get("parent")
                .and_then(Value::as_str)
                .is_none_or(|parent| !nodes.contains_key(parent))
        })
        .map(|(key, _)| key.clone());
    while let Some(key) = at.take() {
        if !visited.insert(key.clone()) || messages.len() >= MAX_MESSAGES {
            break;
        }
        let Some(node) = nodes.get(&key) else { break };
        if let Some(message) = node.get("message").filter(|m| m.is_object()) {
            let author = message.get("author");
            let parts = message
                .pointer("/content/parts")
                .and_then(Value::as_array)
                .map(|parts| {
                    parts
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .or_else(|| string(message.pointer("/content/text")))
                .filter(|text| !text.trim().is_empty());
            messages.push(Message {
                id: string(message.get("id")).or_else(|| Some(key.clone())),
                role: role(
                    author
                        .and_then(|a| a.get("role"))
                        .and_then(Value::as_str)
                        .unwrap_or("unknown"),
                ),
                name: string(author.and_then(|a| a.get("name"))),
                text: parts,
                at: message.get("create_time").and_then(time_of),
            });
        }
        at = node
            .get("children")
            .and_then(Value::as_array)
            .and_then(|children| children.last())
            .and_then(Value::as_str)
            .map(str::to_owned);
    }
    Conversation {
        parser: "tree",
        id: string(raw.get("conversation_id")).or_else(|| string(raw.get("id"))),
        created: raw.get("create_time").and_then(time_of),
        messages,
    }
}

fn flat(raw: &Value) -> Conversation {
    let messages = raw
        .get("messages")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .take(MAX_MESSAGES)
                .map(|message| Message {
                    id: string(message.get("id")).or_else(|| string(message.get("uuid"))),
                    role: role(
                        message
                            .get("sender")
                            .or_else(|| message.get("role"))
                            .and_then(Value::as_str)
                            .unwrap_or("unknown"),
                    ),
                    name: string(message.get("name")),
                    text: string(message.get("text")).or_else(|| string(message.get("content"))),
                    at: message
                        .get("created_at")
                        .or_else(|| message.get("timestamp"))
                        .and_then(time_of),
                })
                .collect()
        })
        .unwrap_or_default();
    Conversation {
        parser: "flat",
        id: string(raw.get("uuid")).or_else(|| string(raw.get("id"))),
        created: raw.get("created_at").and_then(time_of),
        messages,
    }
}
