//! Spec 013 FR-004: `MemoryRepo::insert` cannot be called with a memory that
//! has not been through the gate.
//!
//! The record of 011 is buildable by anyone, which is right: a `Memory` is a
//! value, not a permission. What is not buildable is the permission, and the
//! store's insert asks for that instead.

use aicortex_store::MemoryRepo;
use aicortex_types::{
    Actor, ActorId, Importance, Memory, MemoryBody, MemoryId, MemoryKind, MemoryParts, Provenance,
    Scope, SourceRef, SourceSystem, TrustClass,
};
use rahi_store::{Envelope, TxnBuilder};
use rahi_types::{Revision, Sub, UnixSeconds};

fn main() {
    let at = UnixSeconds::new(1_800_000_000);
    let memory = Memory::new(MemoryParts {
        id: MemoryId::now_v7(),
        scope: Scope::personal(Sub::new("sub-bypass")),
        kind: MemoryKind::Observation,
        body: MemoryBody::text("a memory nobody judged"),
        actor: Actor::human(ActorId::new("bypass").unwrap()),
        provenance: Provenance::captured(
            SourceRef::new(SourceSystem::new("bypass").unwrap()),
            at,
            at,
        ),
        trust: TrustClass::Assertion,
        importance: Importance::at(at).unwrap(),
        created: at,
    });
    let work = Envelope::new("memory", None, memory.id.to_string(), Revision::new(1));
    let mut txn = TxnBuilder::new();
    MemoryRepo::new()
        .insert(&mut txn, &memory, &memory.provenance, &work)
        .unwrap();
}
