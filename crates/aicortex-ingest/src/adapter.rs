//! The one contract every source implements (spec 030 B-1, B-6, B-7).
//!
//! An adapter fetches pages of raw items and maps one raw item to a
//! [`Candidate`]. It never writes to the store, never calls the gate, and
//! never opens a transaction (D-1): everything that follows a mapped
//! candidate belongs to the runner, so a source cannot grow storage logic of
//! its own, which is how the predecessor's forks began.
//!
//! Everything an adapter returns is untrusted input. The runner bounds and
//! validates it ([`RawItem::validate`], [`crate::Cursor::new`]) before using
//! any of it, and the gate judges the candidate like any other capture.
//!
//! The trait is object safe, with boxed futures, because a registry holds
//! adapters by name and concrete adapters arrive in later specs.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use aicortex_types::{MemoryKind, SourceSystem};
use rahi_kernel::{Egress, Governed, Permit, ResourceName};
use rahi_types::{Error, Sub, UnixSeconds};
use serde::{Deserialize, Serialize};

use crate::cursor::Cursor;

/// The future an adapter method returns.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The most items one page may carry.
///
/// One page is one transaction (B-3), so the bound is the transaction's bound.
/// The runner asks for fewer when the item budget is nearly spent, and refuses
/// a page longer than it asked for.
pub const MAX_PAGE_ITEMS: usize = 100;

/// The longest external id, in bytes.
pub const MAX_EXTERNAL_ID_BYTES: usize = 512;

/// The longest locator, in bytes.
pub const MAX_LOCATOR_BYTES: usize = 2_048;

/// The most bytes of options a run request may carry.
pub const MAX_OPTIONS_BYTES: usize = 16 * 1024;

/// A registered source's name.
///
/// The same vocabulary as [`SourceSystem`] (`a-z`, `0-9`, `-`, `_`, `.`, `:`),
/// because it becomes the provenance's source system, a path segment on the
/// run routes, a key in the ingest tables, and part of a lease key.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceId(SourceSystem);

impl SourceId {
    /// Validate and wrap a source name.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when the name is not a well formed source system.
    pub fn new(name: impl Into<String>) -> Result<Self, Error> {
        SourceSystem::new(name)
            .map(Self)
            .map_err(|error| Error::Validation(format!("a source id: {error}")))
    }

    /// The name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// The name as the record's source system.
    #[must_use]
    pub const fn system(&self) -> &SourceSystem {
        &self.0
    }
}

impl core::fmt::Display for SourceId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How well a source establishes where its content came from (B-7).
///
/// The kind decides the default path to the gate. It is a claim about the
/// channel, never about the content, and the gate makes the final call
/// (013 B-7).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// The user's own account, verified: admitted.
    Authenticated,
    /// A file the user uploaded: admitted as an assertion, because nothing
    /// verifies what the file says.
    Export,
    /// A fetched page or a shared channel: quarantined unless the deployment
    /// configures otherwise ([`crate::IngestPolicy`]).
    Public,
}

impl SourceKind {
    /// Every kind.
    pub const ALL: [Self; 3] = [Self::Authenticated, Self::Export, Self::Public];

    /// The stable name.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Authenticated => "authenticated",
            Self::Export => "export",
            Self::Public => "public",
        }
    }
}

/// What a source answers when asked whether a run could start (B-1).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Probe {
    /// What the source calls the account or file it found, for a person's
    /// confirmation. Never content.
    pub label: String,
    /// How many items it expects, when it can say.
    pub estimated_items: Option<u64>,
}

/// One item as the source delivered it, before mapping.
#[derive(Clone, Debug, PartialEq)]
pub struct RawItem {
    /// The source's own id for the item: what an import is idempotent on
    /// (B-4).
    pub external_id: String,
    /// When the source says the item was produced (B-5).
    pub timestamp: UnixSeconds,
    /// Where to look, when the source has such a thing.
    pub locator: Option<String>,
    /// What the source returned, for [`SourceAdapter::map`] alone.
    pub payload: serde_json::Value,
}

impl RawItem {
    /// An item with no locator.
    #[must_use]
    pub fn new(
        external_id: impl Into<String>,
        timestamp: UnixSeconds,
        payload: serde_json::Value,
    ) -> Self {
        Self {
            external_id: external_id.into(),
            timestamp,
            locator: None,
            payload,
        }
    }

    /// The same item, naming where to look.
    #[must_use]
    pub fn with_locator(mut self, locator: impl Into<String>) -> Self {
        self.locator = Some(locator.into());
        self
    }

    /// Whether the fields the runner reads are well formed.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] for an empty, padded, over-long or control
    /// character external id or locator.
    pub fn validate(&self) -> Result<(), Error> {
        check_text("external id", &self.external_id, MAX_EXTERNAL_ID_BYTES)?;
        if let Some(locator) = &self.locator {
            check_text("locator", locator, MAX_LOCATOR_BYTES)?;
        }
        Ok(())
    }
}

fn check_text(what: &str, text: &str, max: usize) -> Result<(), Error> {
    if text.is_empty() || text.trim() != text {
        return Err(Error::Validation(format!(
            "a {what} is not empty and not padded"
        )));
    }
    if text.len() > max {
        return Err(Error::Validation(format!(
            "a {what} is at most {max} bytes, not {}",
            text.len()
        )));
    }
    if text.chars().any(char::is_control) {
        return Err(Error::Validation(format!(
            "a {what} carries no control character"
        )));
    }
    Ok(())
}

/// One page of items and the cursor that continues after it (B-1).
#[derive(Clone, Debug, PartialEq)]
pub struct Page {
    /// The items, at most the page limit the runner asked for.
    pub items: Vec<RawItem>,
    /// Where the next page starts, or `None` when this was the last.
    pub next: Option<Cursor>,
}

/// What an adapter maps an item to, before the gate has judged it.
///
/// Carries what the item says and nothing about where it came from: the
/// runner builds the provenance, the actor, the trust class and the origin
/// (B-5, B-7), so an adapter cannot omit or forge any of them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    /// What kind of claim the item is.
    pub kind: MemoryKind,
    /// A title, when the item has one.
    pub title: Option<String>,
    /// What the item says, as it arrived.
    pub text: String,
}

impl Candidate {
    /// A candidate with no title.
    #[must_use]
    pub fn new(kind: MemoryKind, text: impl Into<String>) -> Self {
        Self {
            kind,
            title: None,
            text: text.into(),
        }
    }

    /// The same candidate, titled.
    #[must_use]
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }
}

/// Why an adapter call did not produce what was asked.
///
/// The text is for the operator's log. It is never stored, never returned to
/// a client and never put in a Decision, because it may quote the source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AdapterError {
    /// Worth trying again: a timeout, a rate limit, a dropped connection.
    Transient(String),
    /// Not worth trying again: a bad credential, a malformed export.
    Fatal(String),
}

impl core::fmt::Display for AdapterError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Transient(message) => write!(f, "transient adapter failure: {message}"),
            Self::Fatal(message) => write!(f, "adapter failure: {message}"),
        }
    }
}

impl std::error::Error for AdapterError {}

/// What an adapter call returns.
pub type AdapterResult<T> = Result<T, AdapterError>;

/// What the runner hands an adapter for one call (B-1, B-6).
///
/// Carries the run's options, the page limit, and the way to reach the
/// network: the kernel's egress facade, restricted to the hosts the adapter
/// declared at registration.
#[derive(Clone, Debug)]
pub struct AdapterContext {
    options: serde_json::Value,
    page_limit: usize,
    hosts: Arc<[ResourceName]>,
    egress: Option<Governed<Egress>>,
    actor: Sub,
}

impl AdapterContext {
    /// A context for one call.
    #[must_use]
    pub fn new(
        options: serde_json::Value,
        page_limit: usize,
        hosts: Arc<[ResourceName]>,
        egress: Option<Governed<Egress>>,
        actor: Sub,
    ) -> Self {
        Self {
            options,
            page_limit,
            hosts,
            egress,
            actor,
        }
    }

    /// What the person who started the run configured the source with.
    #[must_use]
    pub const fn options(&self) -> &serde_json::Value {
        &self.options
    }

    /// The most items the page may carry.
    #[must_use]
    pub const fn page_limit(&self) -> usize {
        self.page_limit
    }

    /// Admission to reach `host`, which the adapter must hold before it
    /// builds a request (B-6).
    ///
    /// Two checks, in order: the host is one the adapter declared at
    /// registration, and the kernel admits it under the ingest service's
    /// `http.egress` grant. The second is the manifest ceiling, and a refusal
    /// is ledgered by the kernel like any other.
    ///
    /// # Errors
    ///
    /// [`AdapterError::Fatal`] when the host was not declared, when the cell
    /// has no egress facade, or when the kernel refuses the host.
    pub async fn permit(&self, host: &str) -> AdapterResult<Permit> {
        if !self
            .hosts
            .iter()
            .any(|declared| declared.matches_host(host))
        {
            return Err(AdapterError::Fatal(format!(
                "the adapter did not declare host {host}"
            )));
        }
        let Some(egress) = &self.egress else {
            return Err(AdapterError::Fatal(format!(
                "this cell has no egress facade for host {host}"
            )));
        };
        egress
            .permit(&self.actor, host)
            .await
            .map_err(|error| AdapterError::Fatal(error.to_string()))
    }
}

/// A source (B-1).
///
/// Implemented once per source, in a module under this crate, and registered
/// by name ([`crate::Registry`]). The methods are plain functions of their
/// inputs apart from the network the context admits: no store, no gate, no
/// transaction, and no clock the runner does not also see.
pub trait SourceAdapter: Send + Sync {
    /// The name this source registers under.
    fn id(&self) -> SourceId;

    /// How well the source establishes where its content came from.
    fn kind(&self) -> SourceKind;

    /// The hosts this adapter reaches, as concrete lowercase names.
    ///
    /// Registration holds each one to the manifest ceiling (B-6, FR-004), so
    /// an adapter whose host is not declared there never starts. The default
    /// is none: an export read from an uploaded file needs no network.
    fn hosts(&self) -> Vec<String> {
        Vec::new()
    }

    /// Check that a run could start, without importing anything.
    fn probe<'a>(&'a self, ctx: &'a AdapterContext) -> BoxFuture<'a, AdapterResult<Probe>>;

    /// Fetch the page that starts at `cursor`, or the first page when there
    /// is none. At most [`AdapterContext::page_limit`] items.
    fn page<'a>(
        &'a self,
        ctx: &'a AdapterContext,
        cursor: Option<&'a Cursor>,
    ) -> BoxFuture<'a, AdapterResult<Page>>;

    /// Map one raw item to the candidate it becomes.
    ///
    /// # Errors
    ///
    /// An item that cannot be mapped is skipped and counted, and does not
    /// stop the run.
    fn map(&self, item: &RawItem) -> AdapterResult<Candidate>;
}
