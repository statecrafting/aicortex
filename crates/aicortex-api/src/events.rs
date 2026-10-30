//! `GET /events`: the progress stream (B-6, FR-007, D-8).
//!
//! A declared streaming route (`rahi://026`): the chassis admits it against
//! the per-identity budget, sends the keep-alive, closes it on overflow, and
//! ends it with its `shutdown` event when the process drains. This module
//! only decides what goes in: events for the caller's own scopes, carrying
//! ids, counts, and states and never a memory body.
//!
//! The bus is node-local. Notify in the store is a single-reader hint stream
//! (`rahi://012` B-3) and cannot fan out to many subscribers, so a stream
//! sees what this node's surface did; a client that reconnects to another
//! node re-reads the listing, which is the source of truth.

use axum::extract::State;
use axum::response::Response;
use rahi_edge::stream::{self, Event, StreamHub};
use rahi_types::Sub;
use serde::Serialize;
use tokio::sync::broadcast;

use crate::router::{Api, Caller};

/// How many events the bus holds for a slow subscriber before it lags.
pub const BUS_CAPACITY: usize = 256;

/// The `event` name a lagging subscriber is told it missed events under; it
/// re-reads the listing.
pub const EVENT_LAGGED: &str = "lagged";

/// One event, as it travels the bus.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiEvent {
    /// The subject whose scope the event concerns. Routing only; never sent.
    pub owner: Sub,
    /// The SSE `event` name.
    pub name: &'static str,
    /// What is sent: ids, counts, and states.
    pub data: EventData,
}

/// The data of an event. Every field is an id, a count, a code, or a state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EventData {
    /// The scope, by kind and key. Never the owner, which the caller knows.
    pub scope: String,
    /// The memory concerned, when there is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The state the memory is now in, or the verdict.
    pub state: String,
    /// The gate's reason code, for a quarantine or a refusal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
}

/// The node's event bus.
#[derive(Clone, Debug)]
pub struct Events {
    tx: broadcast::Sender<ApiEvent>,
}

impl Default for Events {
    fn default() -> Self {
        Self::new()
    }
}

impl Events {
    /// An empty bus.
    #[must_use]
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(BUS_CAPACITY);
        Self { tx }
    }

    /// Publish one event. Nobody listening is not an error.
    pub fn publish(&self, event: ApiEvent) {
        let _ = self.tx.send(event);
    }

    /// Subscribe from now on.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<ApiEvent> {
        self.tx.subscribe()
    }
}

/// The scope label an event carries: kind, and key when it has one.
#[must_use]
pub fn scope_label(scope: &aicortex_types::Scope) -> String {
    scope.kind.key().map_or_else(
        || scope.kind.label().to_owned(),
        |key| format!("{}/{key}", scope.kind.label()),
    )
}

/// `GET /events` (B-6).
///
/// The producer task forwards the caller's events until the chassis cancels
/// the stream: a client gone, an overflow, or the shutdown drain.
pub async fn events(State(api): State<Api>, caller: Caller) -> Response {
    let (producer, receiver) = match api.app.extension::<StreamHub>() {
        Some(hub) => hub.channel(),
        None => stream::channel(stream::DEFAULT_CHANNEL_CAPACITY),
    };
    let mut bus = api.events.subscribe();
    let owner = caller.sub().clone();
    tokio::spawn(async move {
        loop {
            let next = tokio::select! {
                () = producer.cancelled() => return,
                next = bus.recv() => next,
            };
            let event = match next {
                Ok(event) if event.owner == owner => {
                    let data = serde_json::to_string(&event.data).unwrap_or_default();
                    Event::new(event.name, data)
                }
                Ok(_) => continue,
                // No count: the bus carries every scope's events, and how
                // many were missed would say how busy other subjects are.
                Err(broadcast::error::RecvError::Lagged(_)) => Event::new(EVENT_LAGGED, "{}"),
                Err(broadcast::error::RecvError::Closed) => return,
            };
            if producer.emit(event).is_err() {
                return;
            }
        }
    });
    stream::stream(receiver)
}
