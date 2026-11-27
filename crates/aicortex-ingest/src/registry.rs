//! The registered sources (spec 030 B-6, FR-004).
//!
//! A source is registered by name, once, and registration is where its network
//! reach is held to the manifest ceiling: every host the adapter declares must
//! be covered by an `http.egress` grant of the ingest service, or the adapter
//! never enters the registry. The failure is at registration, not at the first
//! fetch, and a new adapter's reach is a reviewable diff in the manifest
//! (`rahi://015`).
//!
//! Concrete adapters (031, 032, 033) add a module under this crate and one
//! registration here, each under an `extends` edge on this file.

use std::collections::BTreeMap;
use std::sync::Arc;

use rahi_kernel::{CapabilityKind, Egress, Governed, Manifest, ResourceName, ServiceName};
use rahi_types::Error;
use serde::Serialize;

use crate::adapter::{SourceAdapter, SourceId, SourceKind};

/// The manifest service whose `http.egress` grants bound every adapter.
pub const INGEST_SERVICE: &str = "ingest";

/// A registered adapter and what was checked about it.
#[derive(Clone)]
pub struct Registered {
    adapter: Arc<dyn SourceAdapter>,
    id: SourceId,
    kind: SourceKind,
    hosts: Arc<[ResourceName]>,
}

impl std::fmt::Debug for Registered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registered")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .field("hosts", &self.hosts)
            .finish_non_exhaustive()
    }
}

impl Registered {
    /// The adapter.
    #[must_use]
    pub fn adapter(&self) -> &Arc<dyn SourceAdapter> {
        &self.adapter
    }

    /// The name it registered under.
    #[must_use]
    pub const fn id(&self) -> &SourceId {
        &self.id
    }

    /// How well it establishes where its content came from.
    #[must_use]
    pub const fn kind(&self) -> SourceKind {
        self.kind
    }

    /// The hosts it declared.
    #[must_use]
    pub fn hosts(&self) -> Arc<[ResourceName]> {
        Arc::clone(&self.hosts)
    }
}

/// What `aicortex preflight` and a listing say about one source.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SourceDescription {
    /// The name.
    pub id: String,
    /// The kind's label.
    pub kind: &'static str,
    /// The declared hosts.
    pub hosts: Vec<String>,
}

/// The sources this process can run, by name.
#[derive(Clone, Debug, Default)]
pub struct Registry {
    sources: BTreeMap<SourceId, Registered>,
}

impl Registry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `adapter`, holding its hosts to the ceiling the kernel
    /// enforces on `egress` (B-6).
    ///
    /// # Errors
    ///
    /// [`Error::Denied`] when a declared host is not covered by the facade's
    /// service; see [`Self::register_under`].
    pub fn register(
        &mut self,
        adapter: Arc<dyn SourceAdapter>,
        egress: &Governed<Egress>,
    ) -> Result<(), Error> {
        self.register_under(adapter, egress.kernel().manifest(), egress.service())
    }

    /// Register `adapter` against a manifest and the service whose grants
    /// bound it.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] for a host that is not a concrete lowercase name
    /// or a source already registered under the name; [`Error::Denied`] when
    /// the manifest has no `http.egress` grant of `service` covering a host.
    pub fn register_under(
        &mut self,
        adapter: Arc<dyn SourceAdapter>,
        manifest: &Manifest,
        service: &ServiceName,
    ) -> Result<(), Error> {
        let id = adapter.id();
        if self.sources.contains_key(&id) {
            return Err(Error::Validation(format!(
                "a source named {id} is already registered"
            )));
        }
        let hosts = declared_hosts(adapter.as_ref())?;
        if let Some(host) = hosts
            .iter()
            .find(|host| !ceiling_covers(manifest, service, host.as_str()))
        {
            return Err(Error::Denied(format!(
                "source {id} declares host {host} and the manifest ceiling grants service \
                 {service} no http.egress covering it"
            )));
        }
        let kind = adapter.kind();
        self.sources.insert(
            id.clone(),
            Registered {
                adapter,
                id,
                kind,
                hosts: hosts.into(),
            },
        );
        Ok(())
    }

    /// Register the sources of spec 031 that read an uploaded file: they
    /// declare no host, so the ceiling is never in question (B-8).
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when one of them is already registered.
    pub fn register_imports(
        &mut self,
        staging: &Arc<crate::upload::Staging>,
        manifest: &Manifest,
        service: &ServiceName,
    ) -> Result<(), Error> {
        let chat = crate::sources::chat_export::ChatExport::new(Arc::clone(staging));
        self.register_under(Arc::new(chat), manifest, service)
    }

    /// The source registered under `id`.
    #[must_use]
    pub fn get(&self, id: &SourceId) -> Option<&Registered> {
        self.sources.get(id)
    }

    /// Every source, in name order.
    pub fn iter(&self) -> impl Iterator<Item = &Registered> {
        self.sources.values()
    }

    /// How many sources are registered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.sources.len()
    }

    /// Whether no source is registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }

    /// Every source and its hosts, in name order.
    #[must_use]
    pub fn describe(&self) -> Vec<SourceDescription> {
        self.iter()
            .map(|source| SourceDescription {
                id: source.id.to_string(),
                kind: source.kind.label(),
                hosts: source
                    .hosts
                    .iter()
                    .map(|host| host.as_str().to_owned())
                    .collect(),
            })
            .collect()
    }

    /// The `(source, host)` pairs that `manifest` no longer covers for
    /// `service`: what preflight reports when a ceiling was narrowed after
    /// the registrations were written.
    #[must_use]
    pub fn uncovered(&self, manifest: &Manifest, service: &ServiceName) -> Vec<(String, String)> {
        self.iter()
            .flat_map(|source| {
                source
                    .hosts
                    .iter()
                    .filter(|host| !ceiling_covers(manifest, service, host.as_str()))
                    .map(|host| (source.id.to_string(), host.as_str().to_owned()))
                    .collect::<Vec<_>>()
            })
            .collect()
    }
}

/// Whether `manifest` grants `service` an `http.egress` capability covering
/// `host`.
#[must_use]
pub fn ceiling_covers(manifest: &Manifest, service: &ServiceName, host: &str) -> bool {
    manifest.covers(service, CapabilityKind::HttpEgress, host)
}

/// An adapter's declared hosts, each a concrete lowercase name.
///
/// A wildcard is refused: the manifest's ceiling may be a pattern, but an
/// adapter names the hosts it reaches, so its reach is the list a reviewer
/// reads and not a pattern the adapter could stretch.
fn declared_hosts(adapter: &dyn SourceAdapter) -> Result<Vec<ResourceName>, Error> {
    let mut hosts = Vec::new();
    for host in adapter.hosts() {
        if host.contains('*') {
            return Err(Error::Validation(format!(
                "source {} declares host {host}; an adapter names concrete hosts",
                adapter.id()
            )));
        }
        let name = ResourceName::parse(host)?;
        if !hosts.contains(&name) {
            hosts.push(name);
        }
    }
    Ok(hosts)
}
