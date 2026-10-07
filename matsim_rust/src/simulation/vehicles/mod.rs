use crate::simulation::Identifiable;
use crate::simulation::agents::SimulationAgentLogic;
use crate::simulation::agents::agent::SimulationAgent;
use crate::simulation::agents::{AgentEvent, EnvironmentalEventObserver};
use crate::simulation::id::Id;
use crate::simulation::scenario::network::Link;
use crate::simulation::scenario::vehicles::InternalVehicle;
use crate::simulation::time::SimTime;

#[derive(Debug)]
pub struct SimulationVehicle {
    pub(super) vehicle: InternalVehicle,
    pub(super) driver: Option<SimulationAgent>,
    pub(super) passengers: Vec<SimulationAgent>,
}

impl SimulationVehicle {
    pub fn new(
        vehicle: InternalVehicle,
        driver: Option<SimulationAgent>,
        passengers: Vec<SimulationAgent>,
    ) -> Self {
        Self {
            vehicle,
            driver,
            passengers,
        }
    }

    #[cfg(test)]
    pub fn from_parts(
        id: u64,
        veh_type: u64,
        max_v: f64,
        pce: f64,
        driver: SimulationAgent,
    ) -> Self {
        Self::new(
            InternalVehicle::new(id, veh_type, max_v, pce),
            Some(driver),
            Vec::new(),
        )
    }

    pub fn driver_mut(&mut self) -> &mut SimulationAgent {
        self.driver
            .as_mut()
            .expect("SimulationVehicle has no driver.")
    }

    pub fn driver(&self) -> &SimulationAgent {
        self.driver
            .as_ref()
            .expect("SimulationVehicle has no driver.")
    }

    pub fn passengers(&self) -> &Vec<SimulationAgent> {
        &self.passengers
    }

    pub fn id(&self) -> &Id<InternalVehicle> {
        &self.vehicle.id
    }

    pub fn max_v(&self) -> f64 {
        self.vehicle.max_v
    }

    pub fn pce(&self) -> f64 {
        self.vehicle.pce
    }

    pub fn curr_link_id(&self) -> Option<&Id<Link>> {
        self.driver().curr_link_id()
    }

    pub fn peek_next_route_element(&self) -> Option<&Id<Link>> {
        self.driver().peek_next_link_id()
    }

    pub fn internal_vehicle(&self) -> &InternalVehicle {
        &self.vehicle
    }

    pub fn into_agents(mut self) -> Vec<SimulationAgent> {
        let mut agents = Vec::with_capacity(1 + self.passengers.len());
        let driver = self
            .driver
            .take()
            .expect("SimulationVehicle has no driver.");
        agents.push(driver);
        agents.extend(self.passengers);
        agents
    }
}

impl EnvironmentalEventObserver for SimulationVehicle {
    fn notify_event(&mut self, event: &mut AgentEvent, now: SimTime) {
        self.driver_mut().notify_event(event, now);
        self.passengers.iter_mut().for_each(|p| {
            p.notify_event(event, now);
        });
    }
}

impl Identifiable<InternalVehicle> for SimulationVehicle {
    fn id(&self) -> &Id<InternalVehicle> {
        self.id()
    }
}
