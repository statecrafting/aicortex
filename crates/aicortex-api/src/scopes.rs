//! The seven OAuth scopes of this surface (B-3, FR-008, D-1, D-2).
//!
//! A scope names what a client was authorised to do on a person's behalf.
//! The chassis's [`RequireScope`] does the refusing, into the decision chain,
//! with the `insufficient_scope` challenge of RFC 6750; this module only says
//! which scope each route asks for. Matching is the chassis's exact string
//! match, so no `memory.*` scope satisfies a `coordination.*` requirement and
//! no `coordination.*` scope satisfies a `memory.*` one: there is no prefix or
//! hierarchy rule here to get wrong.

use std::collections::BTreeSet;

use axum::middleware::from_fn_with_state;
use axum::routing::MethodRouter;
use rahi_idp::RequireScope;
use rahi_kernel::Kernel;

/// One scope a route can require.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ApiScope {
    /// Read memories, their listing, and the counts.
    MemoryRead,
    /// Capture, correct, and supersede.
    MemoryWrite,
    /// Erase.
    MemoryDelete,
    /// Operator-grade work on a scope (D-6: no route of this build uses it).
    MemoryAdmin,
    /// Raise a memory to instruction grade: the human boundary of 019 B-4.
    MemoryPromote,
    /// Coordination views (035, 045).
    CoordinationRead,
    /// Coordination submissions, transitions, and work claims (035, 045).
    CoordinationWrite,
}

impl ApiScope {
    /// Every scope, in the order B-3 lists them.
    pub const ALL: [Self; 7] = [
        Self::MemoryRead,
        Self::MemoryWrite,
        Self::MemoryDelete,
        Self::MemoryAdmin,
        Self::MemoryPromote,
        Self::CoordinationRead,
        Self::CoordinationWrite,
    ];

    /// The scope string a token's `scope` claim carries.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MemoryRead => "memory.read",
            Self::MemoryWrite => "memory.write",
            Self::MemoryDelete => "memory.delete",
            Self::MemoryAdmin => "memory.admin",
            Self::MemoryPromote => "memory.promote",
            Self::CoordinationRead => "coordination.read",
            Self::CoordinationWrite => "coordination.write",
        }
    }

    /// Whether a token granted `granted` reaches a route requiring `self`.
    ///
    /// The same exact membership test the chassis's gate applies, stated here
    /// so the table test of FR-008 can hold the two families apart.
    #[must_use]
    pub fn satisfied_by(self, granted: &BTreeSet<String>) -> bool {
        granted.contains(self.as_str())
    }

    /// The chassis gate for this scope, recording refusals in `kernel`.
    ///
    /// Building it publishes the scope into `scopes_supported`, so only a
    /// scope some mounted route requires is advertised.
    #[must_use]
    pub fn gate(self, kernel: &Kernel) -> RequireScope {
        RequireScope::new(self.as_str(), kernel.clone())
    }
}

/// Gate one method of one route on `scope` (B-3).
///
/// Per method rather than per router, because one path carries several
/// methods with different scopes (`GET`, `PATCH`, and `DELETE` on
/// `/memories/{id}`).
pub fn scoped<S>(scope: ApiScope, kernel: &Kernel, route: MethodRouter<S>) -> MethodRouter<S>
where
    S: Clone + Send + Sync + 'static,
{
    route.layer(from_fn_with_state(
        scope.gate(kernel),
        rahi_idp::scope::enforce_scope,
    ))
}
