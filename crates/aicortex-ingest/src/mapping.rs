//! From a mapped item to a record the gate can judge (spec 030 B-4, B-5, B-7).
//!
//! Everything an adapter could get wrong about a record's origin is built
//! here, from what the runner knows and not from what the adapter says: the
//! source system is the registration's, the external id and timestamp are the
//! item's own, the actor is the run, and the origin and trust class follow
//! from the source's kind and the deployment's policy. An adapter has no way
//! to omit any of them or to forge an actor.

use aicortex_gate::Origin;
use aicortex_store::ScopeId;
use aicortex_types::{
    Actor, ActorId, AgentOrigin, Importance, MemoryBody, MemoryId, MemoryParts, Provenance, Scope,
    SourceRef, TrustClass,
};
use rahi_store::{Statement, StoreHandle, TxnBuilder, Value};
use rahi_types::{EnvReader, Error, UnixSeconds};
use serde::Deserialize;

use crate::adapter::{Author, Candidate, RawItem, SourceId, SourceKind};
use crate::cursor::seconds;

/// A run's identifier: `run-` and a v7 UUID.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RunId(String);

impl RunId {
    /// A fresh id. Time-ordered, so the latest run of a source sorts last.
    #[must_use]
    pub fn mint() -> Self {
        Self(format!("run-{}", uuid::Uuid::now_v7().simple()))
    }

    /// Read an id back, from a route's path or a stored row.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when the text is not exactly what
    /// [`Self::mint`] writes.
    pub fn parse(text: &str) -> Result<Self, Error> {
        let uuid = text
            .strip_prefix("run-")
            .and_then(|rest| uuid::Uuid::parse_str(rest).ok())
            .map(|uuid| format!("run-{}", uuid.simple()))
            .filter(|canonical| canonical == text)
            .ok_or_else(|| {
                Error::Validation("a run id is run- and a 32 digit hex id".to_owned())
            })?;
        Ok(Self(uuid))
    }

    /// The id.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl core::fmt::Display for RunId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a deployment does with a [`SourceKind::Public`] source (B-7).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PublicPolicy {
    /// The items are stored quarantined, for a person to review.
    #[default]
    Quarantine,
    /// The items take the same path as an export.
    Admit,
}

/// The deployment's intake policy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IngestPolicy {
    /// What happens to a public source's items.
    pub public: PublicPolicy,
}

impl IngestPolicy {
    /// The variable that opts a deployment's public sources into admission:
    /// `quarantine` (the default) or `admit`.
    pub const PUBLIC_ORIGIN_ENV: &'static str = "AICORTEX_INGEST_PUBLIC_ORIGIN";

    /// Read the policy from a process environment.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] when the variable names neither choice: a typo must
    /// not decide whether a fetched page is trusted.
    pub fn from_env<E: EnvReader + ?Sized>(env: &E) -> Result<Self, Error> {
        let public = match env.get(Self::PUBLIC_ORIGIN_ENV).as_deref() {
            None | Some("quarantine") => PublicPolicy::Quarantine,
            Some("admit") => PublicPolicy::Admit,
            Some(other) => {
                return Err(Error::Config(format!(
                    "{} is quarantine or admit, not {other:?}",
                    Self::PUBLIC_ORIGIN_ENV
                )));
            }
        };
        Ok(Self { public })
    }

    /// How well the channel establishes the source, and what a client may do
    /// with the memory (B-7).
    ///
    /// A source this deployment registered is attested by the registration
    /// itself, so it is the registered-adapter origin; a public one is the
    /// asserted origin the gate quarantines, unless the policy says
    /// otherwise. No source produces instruction grade: that is a promotion
    /// (019 B-4).
    #[must_use]
    pub const fn admission_for(self, kind: SourceKind) -> (Origin, TrustClass) {
        match kind {
            SourceKind::Authenticated => (Origin::RegisteredAdapter, TrustClass::Evidence),
            SourceKind::Export => (Origin::RegisteredAdapter, TrustClass::Assertion),
            SourceKind::Public => match self.public {
                PublicPolicy::Quarantine => (Origin::Asserted, TrustClass::Assertion),
                PublicPolicy::Admit => (Origin::RegisteredAdapter, TrustClass::Assertion),
            },
        }
    }
}

/// The digest of what an adapter mapped an item to (B-4).
///
/// BLAKE3 over the kind, the title and the text, each length-prefixed so no
/// two candidates share a digest by concatenation. It is stored with the
/// mapping so a re-run can tell an unchanged item from a changed one without
/// reading the memory back; it is content-derived, so erasure sweeps it with
/// the memory (030 D-4).
#[must_use]
pub fn digest(candidate: &Candidate) -> String {
    let mut hasher = blake3::Hasher::new();
    let title = candidate.title.as_deref().unwrap_or("");
    for part in [candidate.kind.label(), title, candidate.text.as_str()] {
        hasher.update(&(part.len() as u64).to_be_bytes());
        hasher.update(part.as_bytes());
    }
    // Who said it and where it sits are part of what was mapped (031), and an
    // item that names neither digests exactly as it did before they existed.
    if candidate.author != Author::Run {
        hasher.update(format!("author:{:?}", candidate.author).as_bytes());
    }
    if let Some(reference) = &candidate.conversation {
        hasher.update(format!("turn:{}:{}", reference.id, reference.turn).as_bytes());
    }
    hasher.finalize().to_hex().to_string()
}

/// The record a mapped item becomes, before the gate judges it (B-5).
///
/// # Errors
///
/// [`Error::Validation`] when the run id or the importance cannot be
/// expressed in the record.
pub fn memory_parts(
    scope: &Scope,
    source: &SourceId,
    run: &RunId,
    item: &RawItem,
    candidate: &Candidate,
    trust: TrustClass,
    now: UnixSeconds,
) -> Result<MemoryParts, Error> {
    let invalid = |error: aicortex_types::TypeError| Error::Validation(error.to_string());
    let mut body = MemoryBody::text(candidate.text.clone());
    if let Some(title) = &candidate.title {
        body = body.with_title(title.clone());
    }
    let mut origin = SourceRef::new(source.system().clone()).with_external_id(&item.external_id);
    if let Some(locator) = &item.locator {
        origin = origin.with_locator(locator);
    }
    Ok(MemoryParts {
        id: MemoryId::now_v7(),
        scope: scope.clone(),
        kind: candidate.kind,
        body,
        actor: actor_of(run, source, &candidate.author)?,
        provenance: {
            let provenance = Provenance::captured(origin, item.timestamp, now);
            match &candidate.conversation {
                Some(reference) => provenance.in_conversation(reference.clone()),
                None => provenance,
            }
        },
        trust,
        importance: Importance::at(now).map_err(invalid)?,
        created: now,
    })
}

/// The actor of a record: the run's, or, for a turn an assistant said, an
/// agent (031 B-4, B-10). Built here from the run and the source, so a source
/// names who spoke and cannot forge who it is.
fn actor_of(run: &RunId, source: &SourceId, author: &Author) -> Result<Actor, Error> {
    let invalid = |error: aicortex_types::TypeError| Error::Validation(error.to_string());
    let id = |suffix: &str| {
        let id = if suffix.is_empty() {
            run.to_string()
        } else {
            format!("{run}/{suffix}")
        };
        ActorId::new(id).map_err(invalid)
    };
    Ok(match author {
        Author::Run => Actor::import(id("")?).labelled(source.as_str()),
        Author::User => Actor::import(id("user")?).labelled("user"),
        Author::Assistant => Actor::agent(
            id("assistant")?,
            AgentOrigin {
                client: Some(source.to_string()),
                model: None,
            },
        )
        .labelled("assistant"),
        Author::Participant(name) => {
            let label: String = name.chars().filter(|c| !c.is_control()).take(80).collect();
            Actor::import(id("participant")?).labelled(label)
        }
    })
}

/// What a source item is mapped to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemRow {
    /// The memory the item became.
    pub memory: MemoryId,
    /// The digest of the candidate it was mapped to.
    pub digest: String,
}

#[derive(Debug, Deserialize)]
struct ItemSql {
    memory_id: String,
    digest: String,
}

/// The `ingest_item` table: what makes an import idempotent (B-4).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ItemRepo;

impl ItemRepo {
    /// The memory an external id was imported as, if it was.
    ///
    /// `query_consistent`, the leader: whether an item is new or a repeat
    /// decides whether a row is inserted or updated, and a stale replica
    /// would import a repeat as new.
    ///
    /// # Errors
    ///
    /// The store's error; [`Error::Integrity`] when the row names an id that
    /// is not a memory id.
    pub async fn lookup(
        store: &StoreHandle,
        scope: &Scope,
        source: &SourceId,
        external_id: &str,
    ) -> Result<Option<ItemRow>, Error> {
        let rows: Vec<ItemSql> = store
            .query_consistent(
                "SELECT memory_id, digest FROM ingest_item
                 WHERE scope_id = $1 AND source_id = $2 AND external_id = $3",
                vec![
                    Value::from(&ScopeId::of(scope)),
                    Value::from(source.as_str()),
                    Value::from(external_id),
                ],
            )
            .await?;
        rows.into_iter()
            .next()
            .map(|row| {
                Ok(ItemRow {
                    memory: MemoryId::parse(&row.memory_id).map_err(|error| {
                        Error::Integrity(format!("an ingest item names a bad memory id: {error}"))
                    })?,
                    digest: row.digest,
                })
            })
            .transpose()
    }

    /// Stage the mapping of an external id to a memory, in the caller's
    /// transaction. An upsert: an update re-points the id at its newest
    /// memory.
    pub fn stage_upsert(
        txn: &mut TxnBuilder,
        scope: &Scope,
        source: &SourceId,
        external_id: &str,
        memory: MemoryId,
        digest: &str,
        at: UnixSeconds,
    ) {
        txn.push(Statement::with_params(
            "INSERT INTO ingest_item (scope_id, source_id, external_id, memory_id, digest, touched)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (scope_id, source_id, external_id) DO UPDATE
                SET memory_id = excluded.memory_id, digest = excluded.digest,
                    touched = excluded.touched",
            vec![
                Value::from(&ScopeId::of(scope)),
                Value::from(source.as_str()),
                Value::from(external_id),
                Value::from(memory.to_string()),
                Value::from(digest),
                Value::Integer(seconds(at)),
            ],
        ));
    }
}
