//! The scope a memory lives in (spec 011 B-2, constitution VII).
//!
//! Every read and every write names a scope. There is no ambient scope, no
//! default scope, and no memory outside one: [`Scope`] has no `Default`, and
//! its owner is rahi's [`Sub`] verbatim rather than an email, a username, or
//! a row in a local table. The predicate spec 012 B-3 puts in every statement
//! is this value; the isolation is only as real as the fact that a caller
//! cannot avoid naming it.

use core::fmt;

use rahi_types::Sub;
use serde::{Deserialize, Deserializer, Serialize};

use crate::error::{Result, TypeError, unknown_variant, validate_key};

/// The vocabulary of [`ScopeKind`], for the refusal message of FR-002.
const SCOPE_KINDS: &[&str] = &["personal", "project", "shared"];

/// An opaque project key: the caller's name for one body of work.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ProjectKey(String);

/// An opaque share key: the name of a scope more than one subject may reach.
///
/// The key names the scope. Who may reach it is a grant table that does not
/// exist yet, and spec 012 B-9 is explicit that it will be a new spec with
/// its own predicate rather than a relaxation of scope isolation.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ShareKey(String);

/// Generate the constructor, accessor, `Display`, and validating
/// `Deserialize` of an opaque key newtype.
///
/// The three are identical but for the field they refuse under, and writing
/// them out three times is how one of them quietly loses its validation.
macro_rules! opaque_key {
    ($type:ident, $field:literal) => {
        impl $type {
            #[doc = concat!("Validate and wrap a `", $field, "`.")]
            ///
            /// # Errors
            ///
            /// [`TypeError::Invalid`] when the key is empty, padded with
            /// whitespace, over the length ceiling, or carries a control
            /// character.
            pub fn new(key: impl Into<String>) -> Result<Self> {
                let key = key.into();
                validate_key($field, &key)?;
                Ok(Self(key))
            }

            /// The key text.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $type {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $type {
            fn deserialize<D: Deserializer<'de>>(
                deserializer: D,
            ) -> core::result::Result<Self, D::Error> {
                let key = String::deserialize(deserializer)?;
                Self::new(key).map_err(serde::de::Error::custom)
            }
        }
    };
}

opaque_key!(ProjectKey, "project");
opaque_key!(ShareKey, "share");

/// Which of the three shapes a scope has.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScopeKind {
    /// The owner's own memory, reachable by nobody else.
    Personal,
    /// One body of work, named by its key.
    Project {
        /// The caller's name for the project.
        project: ProjectKey,
    },
    /// A scope named for sharing, which nothing shares yet.
    Shared {
        /// The name of the share.
        share: ShareKey,
    },
}

impl ScopeKind {
    /// The discriminant, as spec 012 B-2 stores it in the `scope` table.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Personal => "personal",
            Self::Project { .. } => "project",
            Self::Shared { .. } => "shared",
        }
    }

    /// The key, for the two shapes that carry one.
    #[must_use]
    pub fn key(&self) -> Option<&str> {
        match self {
            Self::Personal => None,
            Self::Project { project } => Some(project.as_str()),
            Self::Shared { share } => Some(share.as_str()),
        }
    }
}

/// The wire shape of a [`ScopeKind`], read before it is judged.
///
/// Serde's own untagged and internally-tagged errors name the variant but not
/// the field it sat in; FR-002 wants the field. Reading the discriminant as a
/// plain string first is what lets the refusal say `scope.kind`.
#[derive(Deserialize)]
struct ScopeKindWire {
    kind: String,
    #[serde(default)]
    project: Option<ProjectKey>,
    #[serde(default)]
    share: Option<ShareKey>,
}

impl<'de> Deserialize<'de> for ScopeKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> core::result::Result<Self, D::Error> {
        let wire = ScopeKindWire::deserialize(deserializer)?;
        let kind = match wire.kind.as_str() {
            "personal" => Self::Personal,
            "project" => Self::Project {
                project: wire
                    .project
                    .ok_or(TypeError::MissingField {
                        field: "scope.project",
                        context: "a project scope",
                    })
                    .map_err(serde::de::Error::custom)?,
            },
            "shared" => Self::Shared {
                share: wire
                    .share
                    .ok_or(TypeError::MissingField {
                        field: "scope.share",
                        context: "a shared scope",
                    })
                    .map_err(serde::de::Error::custom)?,
            },
            other => {
                return Err(serde::de::Error::custom(unknown_variant(
                    "scope.kind",
                    other,
                    SCOPE_KINDS,
                )));
            }
        };
        Ok(kind)
    }
}

/// Who owns a body of memory, and which body it is.
///
/// There is no `Default`: a caller that wants a scope names an owner and a
/// kind, which is the whole of B-2.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Scope {
    /// The IdP's subject, verbatim. The only identity this product knows.
    pub owner: Sub,
    /// Which of the three shapes this scope has.
    #[serde(flatten)]
    pub kind: ScopeKind,
}

impl Scope {
    /// The owner's personal scope.
    #[must_use]
    pub fn personal(owner: Sub) -> Self {
        Self {
            owner,
            kind: ScopeKind::Personal,
        }
    }

    /// One project of one owner.
    #[must_use]
    pub fn project(owner: Sub, project: ProjectKey) -> Self {
        Self {
            owner,
            kind: ScopeKind::Project { project },
        }
    }

    /// One share of one owner.
    #[must_use]
    pub fn shared(owner: Sub, share: ShareKey) -> Self {
        Self {
            owner,
            kind: ScopeKind::Shared { share },
        }
    }
}

impl fmt::Display for Scope {
    /// `<sub>/personal`, `<sub>/project/<key>`, `<sub>/shared/<key>`.
    ///
    /// A stable rendering for a log line or an error, never a parsed wire
    /// format: the wire format is the serde one.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.owner.as_str(), self.kind.label())?;
        if let Some(key) = self.kind.key() {
            write!(f, "/{key}")?;
        }
        Ok(())
    }
}
