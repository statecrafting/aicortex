//! Spec 013 FR-004: nor can the permission be forged.
//!
//! `Admitted` has one private field, no public constructor, no `Default`, no
//! `From` and no `Deserialize`. The struct literal is the obvious way in and
//! it is a privacy error; there is no second way.

use aicortex_gate::Admitted;
use aicortex_types::{
    Actor, ActorId, Importance, Memory, MemoryBody, MemoryId, MemoryKind, MemoryParts, Provenance,
    Scope, SourceRef, SourceSystem, TrustClass,
};
use rahi_types::{Sub, UnixSeconds};

fn main() {
    let at = UnixSeconds::new(1_800_000_000);
    let memory = Memory::new(MemoryParts {
        id: MemoryId::now_v7(),
        scope: Scope::personal(Sub::new("sub-forge")),
        kind: MemoryKind::Observation,
        body: MemoryBody::text("a memory nobody judged"),
        actor: Actor::human(ActorId::new("forge").unwrap()),
        provenance: Provenance::captured(
            SourceRef::new(SourceSystem::new("forge").unwrap()),
            at,
            at,
        ),
        trust: TrustClass::Assertion,
        importance: Importance::at(at).unwrap(),
        created: at,
    });
    let forged = Admitted { memory };
    let _ = forged.memory();
}
