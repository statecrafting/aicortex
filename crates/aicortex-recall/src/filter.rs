//! The query filter (spec 018 B-1, B-7).
//!
//! A [`Filter`] is closed and typed. It lowers to an index
//! [`aicortex_index::Filter`]: the status list plus a
//! [`aicortex_index::Restriction`], both rendered into each channel's own
//! statement. There is no post-pass, and quarantined and erased rows are
//! unrequestable by construction: [`Extra`] has no variant for them.

use aicortex_index::{Filter as IndexFilter, Restriction};
use aicortex_store::StatusFilter;
use aicortex_types::{MemoryKind, Status};
use rahi_types::{Error, UnixSeconds};
use serde::Serialize;

/// A status a caller may ask to see beyond the active ones (B-7).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Extra {
    /// Memories a later memory replaced.
    Superseded,
    /// Memories past the moment they were true until.
    Expired,
}

impl Extra {
    /// The `status` column discriminant.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Superseded => "superseded",
            Self::Expired => "expired",
        }
    }
}

/// Trust class labels a filter may name.
const TRUSTS: [&str; 3] = ["evidence", "assertion", "instruction"];
/// Actor kind labels a filter may name.
const ACTOR_KINDS: [&str; 4] = ["human", "agent", "system", "import"];

/// What a query admits beyond its scope. Empty sets admit everything.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Filter {
    /// Memory kind labels.
    pub kinds: Vec<String>,
    /// Trust class labels: `evidence`, `assertion`, `instruction`.
    pub trusts: Vec<String>,
    /// Actor kind labels: `human`, `agent`, `system`, `import`.
    pub actor_kinds: Vec<String>,
    /// Source system names.
    pub sources: Vec<String>,
    /// Entity ids a memory must mention.
    pub entities: Vec<String>,
    /// Earliest `created`, inclusive.
    pub created_from: Option<UnixSeconds>,
    /// Latest `created`, inclusive.
    pub created_to: Option<UnixSeconds>,
    /// Statuses admitted beyond active, recorded in the trace (B-7).
    pub extra_statuses: Vec<Extra>,
}

fn check(field: &str, values: &[String], known: &[&str]) -> Result<(), Error> {
    for value in values {
        if !known.contains(&value.as_str()) {
            return Err(Error::Validation(format!(
                "{value:?} is not a known {field}"
            )));
        }
    }
    Ok(())
}

impl Filter {
    /// Refuse a label outside its vocabulary and an inverted time range.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`].
    pub fn validate(&self) -> Result<(), Error> {
        let kinds: Vec<&str> = MemoryKind::all().iter().map(|kind| kind.label()).collect();
        check("memory kind", &self.kinds, &kinds)?;
        check("trust class", &self.trusts, &TRUSTS)?;
        check("actor kind", &self.actor_kinds, &ACTOR_KINDS)?;
        if let (Some(from), Some(to)) = (self.created_from, self.created_to)
            && from > to
        {
            return Err(Error::Validation(
                "the filter's time range ends before it starts".to_owned(),
            ));
        }
        Ok(())
    }

    /// The status labels the filter admits, sorted: active, then any extras.
    #[must_use]
    pub fn status_labels(&self) -> Vec<&'static str> {
        let mut labels = vec![Status::Active.label()];
        labels.extend(self.extra_statuses.iter().map(|extra| extra.label()));
        labels.sort_unstable();
        labels.dedup();
        labels
    }

    /// The predicates beyond status, as the index renders them.
    #[must_use]
    pub fn restriction(&self) -> Restriction {
        Restriction {
            kinds: self.kinds.clone(),
            trusts: self.trusts.clone(),
            actor_kinds: self.actor_kinds.clone(),
            sources: self.sources.clone(),
            entities: self.entities.clone(),
            created_from: self.created_from.map(UnixSeconds::get),
            created_to: self.created_to.map(UnixSeconds::get),
        }
    }

    /// The filter an index channel runs with.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] from [`Filter::validate`].
    pub fn index_filter(&self) -> Result<IndexFilter, Error> {
        self.validate()?;
        // Built from the same set as `status_labels`, exhaustively: a new
        // `Extra` variant does not compile until every channel admits it.
        let statuses =
            std::iter::once(StatusFilter::Active).chain(self.extra_statuses.iter().map(|extra| {
                match extra {
                    Extra::Superseded => StatusFilter::Superseded,
                    Extra::Expired => StatusFilter::Expired,
                }
            }));
        Ok(IndexFilter::status_filters(statuses)?.restricted(self.restriction()))
    }
}
