//! B-8, FR-004: the parts a memory is built from cannot be written without a
//! provenance. The field is required, there is no `Default`, and `Memory`
//! itself is `#[non_exhaustive]`, so this literal is the only builder and it
//! does not compile with the field left out.

use aicortex_types::{Memory, MemoryParts};

fn build(other: MemoryParts) -> Memory {
    Memory::new(MemoryParts {
        id: other.id,
        scope: other.scope,
        kind: other.kind,
        body: other.body,
        actor: other.actor,
        trust: other.trust,
        importance: other.importance,
        created: other.created,
    })
}

fn main() {
    let _ = build;
}
