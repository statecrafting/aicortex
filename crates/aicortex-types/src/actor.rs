//! Who made the claim (spec 011 B-7).
//!
//! A memory is somebody's claim, and the somebody is named. Four kinds cover
//! the ways a claim reaches this system: a person, an agent acting for a
//! person, the product itself, and an import of a history that predates the
//! product. An [`ActorId`] is deliberately not a [`rahi_types::Sub`]: only a
//! person has a subject, and keeping the two types apart is what makes the
//! promotion of spec 011 B-5 unreachable from agent code.

use core::fmt;

use serde::{Deserialize, Deserializer, Serialize};

use crate::error::{Result, unknown_variant, validate_key};

/// The vocabulary of [`ActorKind`], for the refusal message of FR-002.
const ACTOR_KINDS: &[&str] = &["human", "agent", "system", "import"];

/// The name of an actor within its kind.
///
/// For a human this is the IdP's subject rendered as text; for an agent, the
/// coordination identity spec 035 issues; for the system, the component; for
/// an import, the run. It is text because the four namespaces do not share a
/// shape, and it is a distinct type from `Sub` because an agent must never be
/// mistaken for the person it acts for.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ActorId(String);

impl ActorId {
    /// Validate and wrap an actor name.
    ///
    /// # Errors
    ///
    /// [`TypeError::Invalid`] naming `actor.id` when the name is empty,
    /// padded, over the length ceiling, or carries a control character.
    pub fn new(id: impl Into<String>) -> Result<Self> {
        let id = id.into();
        validate_key("actor.id", &id)?;
        Ok(Self(id))
    }

    /// The name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ActorId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ActorId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> core::result::Result<Self, D::Error> {
        let id = String::deserialize(deserializer)?;
        Self::new(id).map_err(serde::de::Error::custom)
    }
}

/// The client and model behind an agent's claim, when they are known.
///
/// Known is the operative word: an agent that reaches the product over MCP
/// announces itself, and an agent that does not is recorded without the
/// announcement rather than with a guess.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct AgentOrigin {
    /// The client the agent ran in, such as a coding CLI or an editor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<String>,
    /// The model that produced the claim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

impl AgentOrigin {
    /// Whether anything at all is known about the origin.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.client.is_none() && self.model.is_none()
    }
}

/// What kind of thing made the claim.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ActorKind {
    /// A person, authenticated by the IdP.
    Human,
    /// An agent acting for a person, with its client and model when known.
    Agent {
        /// What is known about where the agent ran.
        #[serde(default, skip_serializing_if = "AgentOrigin::is_empty")]
        origin: AgentOrigin,
    },
    /// The product itself: a curator, a worker, a migration.
    System,
    /// A history brought in from somewhere else (spec 031).
    Import,
}

impl ActorKind {
    /// The discriminant, as spec 012 stores it.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::Agent { .. } => "agent",
            Self::System => "system",
            Self::Import => "import",
        }
    }
}

/// The wire shape of an [`ActorKind`], read as a string first so FR-002's
/// refusal can name the field.
#[derive(Deserialize)]
struct ActorKindWire {
    kind: String,
    #[serde(default)]
    origin: Option<AgentOrigin>,
}

impl<'de> Deserialize<'de> for ActorKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> core::result::Result<Self, D::Error> {
        let wire = ActorKindWire::deserialize(deserializer)?;
        match wire.kind.as_str() {
            "human" => Ok(Self::Human),
            "agent" => Ok(Self::Agent {
                origin: wire.origin.unwrap_or_default(),
            }),
            "system" => Ok(Self::System),
            "import" => Ok(Self::Import),
            other => Err(serde::de::Error::custom(unknown_variant(
                "actor.kind",
                other,
                ACTOR_KINDS,
            ))),
        }
    }
}

/// Who made the claim: a kind, a name within that kind, and a label a person
/// can read.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Actor {
    /// What kind of thing this is.
    #[serde(flatten)]
    pub kind: ActorKind,
    /// The name within that kind.
    pub id: ActorId,
    /// A display name, when there is one worth showing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl Actor {
    /// A person.
    #[must_use]
    pub const fn human(id: ActorId) -> Self {
        Self {
            kind: ActorKind::Human,
            id,
            label: None,
        }
    }

    /// An agent, with whatever is known about where it ran.
    #[must_use]
    pub const fn agent(id: ActorId, origin: AgentOrigin) -> Self {
        Self {
            kind: ActorKind::Agent { origin },
            id,
            label: None,
        }
    }

    /// The product itself.
    #[must_use]
    pub const fn system(id: ActorId) -> Self {
        Self {
            kind: ActorKind::System,
            id,
            label: None,
        }
    }

    /// An import run.
    #[must_use]
    pub const fn import(id: ActorId) -> Self {
        Self {
            kind: ActorKind::Import,
            id,
            label: None,
        }
    }

    /// The same actor, with a display name.
    #[must_use]
    pub fn labelled(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Whether this claim was made by a person.
    #[must_use]
    pub const fn is_human(&self) -> bool {
        matches!(self.kind, ActorKind::Human)
    }
}

impl fmt::Display for Actor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.kind.label(), self.id)
    }
}
