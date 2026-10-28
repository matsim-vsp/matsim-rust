use crate::simulation::Identifiable;
use crate::simulation::agents::agent_logic::{
    AdaptivePlanBasedSimulationLogic, PlanBasedSimulationLogic,
};
use crate::simulation::agents::{
    ActivityLogic, ActivityMut, AgentPhase, AgentPhaseMut, LegLogic, LegMut, NetworkDriverLogic,
    SimulationAgentLogic, TeleportationLogic,
};
use crate::simulation::id::Id;
use crate::simulation::scenario::population::InternalPerson;

#[derive(Debug)]
pub struct SimulationAgent {
    logic: Box<dyn SimulationAgentLogic>,
}

impl PartialEq for SimulationAgent {
    fn eq(&self, other: &Self) -> bool {
        self.id() == other.id()
    }
}

impl SimulationAgent {
    pub fn new_plan_based(person: InternalPerson) -> Self {
        Self::new(Box::new(PlanBasedSimulationLogic::new(person)))
    }

    pub fn new_adaptive_plan_based(person: InternalPerson) -> Self {
        Self::new(Box::new(AdaptivePlanBasedSimulationLogic::new(person)))
    }

    pub fn new(logic: Box<dyn SimulationAgentLogic>) -> Self {
        Self { logic }
    }

    pub fn into_person(self) -> InternalPerson {
        self.logic.into_person()
    }

    /// Convenience projections keep phase/role diagnostics at the capability boundary.
    pub fn activity(&self) -> &dyn ActivityLogic {
        match self.phase() {
            AgentPhase::Activity(activity) => activity,
            AgentPhase::Leg(_) => self.unexpected_phase("Activity", "Leg"),
        }
    }
    pub fn leg(&self) -> &dyn LegLogic {
        match self.phase() {
            AgentPhase::Leg(leg) => leg,
            AgentPhase::Activity(_) => self.unexpected_phase("Leg", "Activity"),
        }
    }

    // Resolving the id costs a dynamic call, so it happens only on the failure path.
    #[cold]
    fn unexpected_phase(&self, expected: &str, found: &str) -> ! {
        panic!(
            "Agent {}: expected {expected} phase, found {found}",
            self.id()
        )
    }

    pub fn activity_mut(&mut self) -> ActivityMut<'_> {
        self.phase_mut().expect_activity()
    }

    pub fn leg_mut(&mut self) -> LegMut<'_> {
        self.phase_mut().expect_leg()
    }

    pub fn network_driver(&self) -> &dyn NetworkDriverLogic {
        self.leg()
            .network_driver()
            .unwrap_or_else(|| panic!("Agent {}: expected NetworkDriver role", self.id()))
    }

    pub fn network_driver_mut(&mut self) -> &mut dyn NetworkDriverLogic {
        let leg = self.leg_mut();
        let id = leg.agent_id();
        leg.into_network_driver().unwrap_or_else(|| {
            panic!(
                "Agent {}: expected NetworkDriver role",
                Id::<InternalPerson>::get(id)
            )
        })
    }

    pub fn teleportation(&self) -> &dyn TeleportationLogic {
        self.leg()
            .teleportation()
            .unwrap_or_else(|| panic!("Agent {}: expected Teleportation role", self.id()))
    }

    pub fn teleportation_mut(&mut self) -> &mut dyn TeleportationLogic {
        let leg = self.leg_mut();
        let id = leg.agent_id();
        leg.into_teleportation().unwrap_or_else(|| {
            panic!(
                "Agent {}: expected Teleportation role",
                Id::<InternalPerson>::get(id)
            )
        })
    }

    pub fn phase(&self) -> AgentPhase<'_> {
        self.logic.phase()
    }
    fn phase_mut(&mut self) -> AgentPhaseMut<'_> {
        self.logic.phase_mut()
    }
}

impl Identifiable<InternalPerson> for SimulationAgent {
    fn id(&self) -> &Id<InternalPerson> {
        self.logic.id()
    }
}
