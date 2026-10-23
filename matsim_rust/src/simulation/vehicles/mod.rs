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

    /// Creates a vehicle of schedule-based transit. It has no driver, because the transit engine
    /// moves it.
    pub fn new_transit(vehicle: InternalVehicle) -> Self {
        Self::new(vehicle, None, Vec::new())
    }

    pub fn add_passenger(&mut self, passenger: SimulationAgent) {
        self.passengers.push(passenger);
    }

    /// Removes and returns the passengers for which `alights` is true, keeping the order of both
    /// the remaining and the removed passengers.
    pub fn take_passengers_where(
        &mut self,
        mut alights: impl FnMut(&SimulationAgent) -> bool,
    ) -> Vec<SimulationAgent> {
        if !self.passengers.iter().any(&mut alights) {
            return Vec::new();
        }
        let (alighting, staying) = std::mem::take(&mut self.passengers)
            .into_iter()
            .partition(|passenger| alights(passenger));
        self.passengers = staying;
        alighting
    }

    pub fn into_passengers(self) -> Vec<SimulationAgent> {
        self.passengers
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

#[cfg(test)]
mod tests {
    use super::SimulationVehicle;
    use crate::simulation::Identifiable;
    use crate::simulation::scenario::vehicles::InternalVehicle;
    use crate::test_utils::create_agent;
    use macros::deterministic_id_test;

    #[deterministic_id_test]
    fn take_passengers_where_keeps_order_of_both_groups() {
        let mut vehicle = SimulationVehicle::new_transit(InternalVehicle::new(1, 0, 1.0, 1.0));
        for id in 1..=5 {
            vehicle.add_passenger(create_agent(id, vec!["destination"]));
        }

        let alighting = vehicle.take_passengers_where(|p| p.id().external() != "3");
        let ids = |agents: &[_]| -> Vec<String> {
            agents
                .iter()
                .map(|a: &crate::simulation::agents::agent::SimulationAgent| {
                    a.id().external().to_owned()
                })
                .collect()
        };

        assert_eq!(vec!["1", "2", "4", "5"], ids(&alighting));
        assert_eq!(vec!["3"], ids(vehicle.passengers()));
        assert!(vehicle.take_passengers_where(|_| false).is_empty());
        assert_eq!(1, vehicle.into_passengers().len());
    }
}
