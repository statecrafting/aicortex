//! B-5, FR-003: `Promotion::new` takes a human subject. An agent carries an
//! `ActorId`, which is a different type, so an agent cannot be passed here and
//! cannot promote its own memory on any code path.

use aicortex_types::{Actor, ActorId, AgentOrigin, DecisionRef, Promotion};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let agent = Actor::agent(ActorId::new("agent:reviewer")?, AgentOrigin::default());
    let decision = DecisionRef::new("01JC5X6Q0000000000000000")?;
    let _ = Promotion::new(agent, decision);
    Ok(())
}
