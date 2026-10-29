use crate::simulation::Identifiable;
use crate::simulation::agents::{ActivityAgent, LegAgent};
use crate::simulation::id::Id;
use crate::simulation::scenario::population::InternalPerson;

/// A heterogeneous owner for cleanup and population materialization only.
/// Engine queues keep their specialized owned capabilities instead.
#[derive(Debug)]
pub enum SimulationAgent {
    Activity(ActivityAgent),
    Leg(LegAgent),
}

impl PartialEq for SimulationAgent {
    fn eq(&self, other: &Self) -> bool {
        self.id() == other.id()
    }
}

impl SimulationAgent {
    pub fn into_person(self) -> Option<InternalPerson> {
        match self {
            Self::Activity(agent) => agent.into_person(),
            Self::Leg(agent) => agent.into_person(),
        }
    }
}

impl Identifiable<InternalPerson> for SimulationAgent {
    fn id(&self) -> &Id<InternalPerson> {
        match self {
            Self::Activity(agent) => agent.id(),
            Self::Leg(agent) => agent.id(),
        }
    }
}
