//! The closed vocabularies: entity kinds, predicates, and edge status
//! (spec 017 B-1, B-4, B-5, D-1).
//!
//! Both enums are closed and have a stable string form. An extractor that
//! proposes a string outside [`Predicate`] has its edge rejected and counted
//! (B-5); nothing here stores free text. Adding a predicate is a migration
//! and a spec amendment, which is what keeps the graph queryable.

use core::fmt;

use serde::{Deserialize, Serialize};

/// What an entity is (B-1). Closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityKind {
    /// A human.
    Person,
    /// A company, team, or institution.
    Organization,
    /// A body of work with a name.
    Project,
    /// A location.
    Place,
    /// A document, file, package, or other made thing.
    Artifact,
    /// An idea, topic, or preference.
    Concept,
    /// Something that happened.
    Event,
}

impl EntityKind {
    /// Every kind, in the order the spec lists them.
    pub const ALL: [Self; 7] = [
        Self::Person,
        Self::Organization,
        Self::Project,
        Self::Place,
        Self::Artifact,
        Self::Concept,
        Self::Event,
    ];

    /// The stored label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Person => "person",
            Self::Organization => "organization",
            Self::Project => "project",
            Self::Place => "place",
            Self::Artifact => "artifact",
            Self::Concept => "concept",
            Self::Event => "event",
        }
    }

    /// The kind a stored label names, if any.
    #[must_use]
    pub fn parse(label: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.label() == label)
    }
}

impl fmt::Display for EntityKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// The relations an edge may state (B-5). Closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Predicate {
    /// A person or team works on a project.
    WorksOn,
    /// A person works at an organization.
    WorksAt,
    /// A person or organization belongs to an organization.
    MemberOf,
    /// A thing is located in a place.
    LocatedIn,
    /// A person made an artifact.
    Authored,
    /// One thing needs another.
    DependsOn,
    /// One thing is a component of another.
    PartOf,
    /// A weaker, untyped association.
    RelatedTo,
    /// A subject prefers something (a literal).
    Prefers,
    /// A subject decided something (a literal).
    Decided,
    /// One event or thing caused another.
    Caused,
    /// One thing replaces another.
    Supersedes,
}

/// What may stand on one side of a predicate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    /// An entity of this kind.
    Entity(EntityKind),
    /// Literal text.
    Literal,
}

impl Predicate {
    /// Every predicate, in the order the spec lists them.
    pub const ALL: [Self; 12] = [
        Self::WorksOn,
        Self::WorksAt,
        Self::MemberOf,
        Self::LocatedIn,
        Self::Authored,
        Self::DependsOn,
        Self::PartOf,
        Self::RelatedTo,
        Self::Prefers,
        Self::Decided,
        Self::Caused,
        Self::Supersedes,
    ];

    /// The stable string form (B-5).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WorksOn => "works_on",
            Self::WorksAt => "works_at",
            Self::MemberOf => "member_of",
            Self::LocatedIn => "located_in",
            Self::Authored => "authored",
            Self::DependsOn => "depends_on",
            Self::PartOf => "part_of",
            Self::RelatedTo => "related_to",
            Self::Prefers => "prefers",
            Self::Decided => "decided",
            Self::Caused => "caused",
            Self::Supersedes => "supersedes",
        }
    }

    /// The predicate a string names, or `None`: the vocabulary check (B-5).
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.as_str() == text)
    }

    /// The kind the default extractor gives a subject of this predicate.
    #[must_use]
    pub const fn subject_kind(self) -> EntityKind {
        match self {
            Self::WorksOn
            | Self::WorksAt
            | Self::MemberOf
            | Self::Authored
            | Self::Prefers
            | Self::Decided => EntityKind::Person,
            Self::LocatedIn => EntityKind::Organization,
            Self::DependsOn | Self::PartOf => EntityKind::Project,
            Self::RelatedTo => EntityKind::Concept,
            Self::Caused => EntityKind::Event,
            Self::Supersedes => EntityKind::Artifact,
        }
    }

    /// What the default extractor expects as the object.
    #[must_use]
    pub const fn object_slot(self) -> Slot {
        match self {
            Self::WorksOn | Self::DependsOn | Self::PartOf => Slot::Entity(EntityKind::Project),
            Self::WorksAt | Self::MemberOf => Slot::Entity(EntityKind::Organization),
            Self::LocatedIn => Slot::Entity(EntityKind::Place),
            Self::Authored | Self::Supersedes => Slot::Entity(EntityKind::Artifact),
            Self::RelatedTo => Slot::Entity(EntityKind::Concept),
            Self::Caused => Slot::Entity(EntityKind::Event),
            Self::Prefers | Self::Decided => Slot::Literal,
        }
    }
}

impl fmt::Display for Predicate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An edge's standing (B-4, FR-003, FR-006).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeStatus {
    /// Believed, and routable by a walk.
    Active,
    /// Replaced by a later extractor version's edge for the same memory.
    Superseded,
    /// Its last source memory was erased (FR-006).
    OriginErased,
}

impl EdgeStatus {
    /// The stored label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Superseded => "superseded",
            Self::OriginErased => "origin_erased",
        }
    }

    /// The status a stored label names, if any.
    #[must_use]
    pub fn parse(label: &str) -> Option<Self> {
        [Self::Active, Self::Superseded, Self::OriginErased]
            .into_iter()
            .find(|status| status.label() == label)
    }
}
