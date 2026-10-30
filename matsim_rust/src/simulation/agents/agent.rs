use crate::simulation::Identifiable;
use crate::simulation::agents::{ActivityAgent, LegAgent, StuckAgent};
use crate::simulation::id::Id;
use crate::simulation::scenario::population::{InternalPerson, InternalPlan};

/// A heterogeneous owner for cleanup and population materialization only.
/// Engine queues keep their specialized owned capabilities instead.
#[derive(Debug)]
pub enum SimulationAgent {
    Activity(ActivityAgent),
    Leg(LegAgent),
    Stuck(StuckAgent),
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
            Self::Stuck(agent) => agent.into_person(),
        }
    }
}

impl Identifiable<InternalPerson> for SimulationAgent {
    fn id(&self) -> &Id<InternalPerson> {
        match self {
            Self::Activity(agent) => agent.id(),
            Self::Leg(agent) => agent.id(),
            Self::Stuck(agent) => agent.id(),
        }
    }
}

/// The person of an agent during mobsim. Plan changes apply to the current iteration only, so the
/// agent can read its person but not change it. An agent that replans works on a copy of its plan.
#[derive(Debug, PartialEq, Clone)]
pub(in crate::simulation::agents) struct ReadOnlyPerson(InternalPerson);

impl ReadOnlyPerson {
    pub(in crate::simulation::agents) fn new(person: InternalPerson) -> Self {
        Self(person)
    }

    pub(in crate::simulation::agents) fn id(&self) -> &Id<InternalPerson> {
        self.0.id()
    }

    pub(in crate::simulation::agents) fn plans(&self) -> &[InternalPlan] {
        self.0.plans()
    }

    /// Hands the person back after mobsim.
    pub(in crate::simulation::agents) fn into_inner(self) -> InternalPerson {
        self.0
    }
}
