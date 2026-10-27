use crate::simulation::Identifiable;
use crate::simulation::agents::agent_logic::{
    AdaptivePlanBasedSimulationLogic, PlanBasedSimulationLogic,
};
use crate::simulation::agents::{
    ActivityLogic, ActivityMut, AgentPhase, AgentPhaseMut, LegLogic, LegMut, NetworkDriverCommands,
    NetworkDriverLogic, SimulationAgentLogic, TeleportationCommands, TeleportationLogic,
    VehiclePassengerCommands,
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
    #[inline]
    pub fn activity(&self) -> &dyn ActivityLogic {
        match self.phase() {
            AgentPhase::Activity(activity) => activity,
            AgentPhase::Leg(_) => panic!("Agent {}: expected Activity phase, found Leg", self.id()),
        }
    }
    #[inline]
    pub fn leg(&self) -> &dyn LegLogic {
        match self.phase() {
            AgentPhase::Leg(leg) => leg,
            AgentPhase::Activity(_) => {
                panic!("Agent {}: expected Leg phase, found Activity", self.id())
            }
        }
    }

    pub fn activity_mut(&mut self) -> ActivityMut<'_> {
        self.phase_mut().expect_activity()
    }

    #[inline]
    pub fn leg_mut(&mut self) -> LegMut<'_> {
        self.phase_mut().expect_leg()
    }

    #[inline]
    pub fn network_driver(&self) -> &dyn NetworkDriverLogic {
        self.leg()
            .network_driver()
            .unwrap_or_else(|| panic!("Agent {}: expected NetworkDriver role", self.id()))
    }

    #[inline]
    pub fn network_driver_mut(&mut self) -> &mut dyn NetworkDriverCommands {
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

    pub fn teleportation_mut(&mut self) -> &mut dyn TeleportationCommands {
        let leg = self.leg_mut();
        let id = leg.agent_id();
        leg.into_teleportation().unwrap_or_else(|| {
            panic!(
                "Agent {}: expected Teleportation role",
                Id::<InternalPerson>::get(id)
            )
        })
    }
    pub fn vehicle_passenger_mut(&mut self) -> &mut dyn VehiclePassengerCommands {
        let leg = self.leg_mut();
        let id = leg.agent_id();
        leg.into_vehicle_passenger().unwrap_or_else(|| {
            panic!(
                "Agent {}: expected VehiclePassenger role",
                Id::<InternalPerson>::get(id)
            )
        })
    }

    #[inline]
    pub(crate) fn phase(&self) -> AgentPhase<'_> {
        self.logic.phase()
    }
    #[inline]
    fn phase_mut(&mut self) -> AgentPhaseMut<'_> {
        self.logic.phase_mut()
    }
}

impl Identifiable<InternalPerson> for SimulationAgent {
    fn id(&self) -> &Id<InternalPerson> {
        self.logic.id()
    }
}
