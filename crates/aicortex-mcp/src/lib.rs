//! The MCP server (spec 021): one remote server over streamable HTTP, stateless
//! per request, serving every client that speaks the protocol.
//!
//! The cell mounts [`router`] and declares [`bearer_routes`]; the chassis
//! supplies the resource server, so a request with no credential is answered
//! with the 401 challenge that lets a client discover the authorization server
//! (B-4) and this crate holds no authorization logic of its own. Each tool is
//! a translation into a call to the API's router (spec 020) as the principal
//! the chassis resolved, so the scope gate that decides is that route's.
//!
//! - [`transport`]: the endpoint, `Accept` negotiation, JSON or a stream.
//! - [`session`]: protocol revisions, derived per request, with no state.
//! - [`tools`]: the seven tools, validated before any work.
//! - [`resources`]: read-only counts, never a memory body.
//! - [`compat`]: the documented list of what named clients need.
//! - [`render`]: JSON-RPC envelopes and tool results.
//!
//! No tool description, schema, or resource carries a string read from the
//! store (019 B-3). Memory bodies leave only as the API's already framed
//! response: envelopes and the framing statement.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod compat;
pub mod render;
pub mod resources;
pub mod session;
pub mod tools;
pub mod transport;

use rahi_idp::BearerRoutes;

pub use compat::COMPAT_PATH;
pub use session::Revision;
pub use transport::{MCP_PATH, Mcp, router};

/// `routes` with the MCP endpoint declared a bearer route (B-4).
///
/// Service-callable like the API it translates (020 D-5): an agent harness
/// holding a client credentials token reaches the tools its scopes name.
/// There is no promotion tool (D-2), so no tool needs a person's token that
/// the API route behind it would not demand itself.
#[must_use]
pub fn bearer_routes(routes: BearerRoutes) -> BearerRoutes {
    routes.service_route(MCP_PATH)
}
