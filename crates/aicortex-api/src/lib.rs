//! The product's own HTTP surface (spec 020), mounted by the cell into
//! rahi's edge.
//!
//! The predecessor authorized everything with one static key in a query
//! string (`openbrain://static-shared-key`). Here every route is reached with
//! a rauthy-issued credential that the chassis validates, resolved to rahi's
//! `Principal`, and gated by an OAuth scope that names what it permits
//! ([`scopes`]). This crate mints no credential, holds no key, and runs no
//! server: the cell returns [`router`] from `Cell::routes` and declares
//! [`bearer_routes`] from `Cell::bearer_routes`, and the chassis does the
//! rest.
//!
//! - [`router`]: the routes under `/api/v1`, each behind its scope gate.
//! - [`capture`]: `POST /capture`, through the write gate, idempotent.
//! - [`events`]: `GET /events`, the chassis-declared progress stream.
//! - [`dto`]: the wire types; inputs reject unknown fields.
//! - [`error`]: RFC 9457 problem documents with a stable type per reason.
//!
//! Memory bodies leave only inside the envelope of spec 019; no type here
//! carries a body any other way.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod capture;
pub mod dto;
pub mod error;
pub mod events;
pub mod router;
pub mod scopes;

pub use error::{ApiError, ApiResult, Problem};
pub use events::{ApiEvent, Events};
pub use router::{
    API_PREFIX, Api, Caller, Limits, PROMOTE_PREFIX, bearer_routes, router, router_with,
};
pub use scopes::ApiScope;
