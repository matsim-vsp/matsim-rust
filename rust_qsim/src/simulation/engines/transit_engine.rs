//! Starts transit drivers on schedule and puts departing passengers at their stops. Port of
//! MATSim's `TransitQSimEngine`; what happens at a stop lives in `pt::driver`.

use crate::simulation::Identifiable;
use crate::simulation::agents::SimulationAgentLogic;
use crate::simulation::agents::agent::SimulationAgent;
use crate::simulation::controller::ThreadLocalComputationalEnvironment;
use crate::simulation::events::{
    AgentWaitingForPtEventBuilder, PersonDepartureEventBuilder, PersonEntersVehicleEventBuilder,
};
use crate::simulation::id::Id;
use crate::simulation::pt::driver::TransitDriver;
use crate::simulation::pt::stops::{TransitStops, WaitingPassenger};
use crate::simulation::scenario::ScenarioCore;
use crate::simulation::scenario::population::InternalPerson;
use crate::simulation::scenario::transit::{TransitLine, TransitSchedule, TransitStopFacility};
use crate::simulation::scenario::vehicles::Garage;
use crate::simulation::time::{SimClock, SimTime, Tick};
use crate::simulation::time_queue::TimeQueue;
use crate::simulation::vehicles::SimulationVehicle;
use nohash_hasher::IntSet;
use std::sync::Arc;

pub(crate) struct TransitEngine {
    /// Drivers between two legs, woken at their next departure.
    waiting_drivers: TimeQueue<SimulationAgent, InternalPerson>,
    /// Drivers that have driven their whole run.
    finished_drivers: Vec<SimulationAgent>,
    transit_modes: IntSet<Id<String>>,
    schedule: Arc<TransitSchedule>,
    garage: Arc<Garage>,
    comp_env: ThreadLocalComputationalEnvironment,
    clock: SimClock,
}

impl TransitEngine {
    /// Creates the drivers of the runs that start on partition `rank`.
    pub(crate) fn new(
        scenario: &ScenarioCore,
        rank: u32,
        comp_env: ThreadLocalComputationalEnvironment,
        clock: SimClock,
        start: SimTime,
    ) -> Self {
        let mut engine = Self {
            waiting_drivers: TimeQueue::new(),
            finished_drivers: Vec::new(),
            transit_modes: scenario
                .config
                .transit()
                .transit_modes
                .iter()
                .map(|mode| Id::get_from_ext(mode))
                .collect(),
            schedule: scenario.transit_schedule.clone(),
            garage: scenario.garage.clone(),
            comp_env,
            clock,
        };
        let runs = scenario.transit_runs.runs().iter();
        for run in runs.filter(|run| scenario.network.get_link(run.start_link()).partition == rank)
        {
            let driver =
                SimulationAgent::new(Box::new(TransitDriver::new(run.clone(), &engine.garage)));
            engine.wait_for_departure(driver, start);
        }
        engine
    }

    pub(crate) fn serves(&self, mode: &Id<String>) -> bool {
        self.transit_modes.contains(mode)
    }

    pub(crate) fn drain(&mut self) -> Vec<SimulationAgent> {
        self.waiting_drivers
            .drain()
            .into_iter()
            .chain(self.finished_drivers.drain(..))
            .collect()
    }

    fn wait_for_departure(&mut self, driver: SimulationAgent, now: SimTime) {
        let order = driver.id().internal();
        self.waiting_drivers.add_with_order(driver, now, order);
    }

    /// Starts the legs of all drivers due now and returns their vehicles, which enter the
    /// network at the start of their route.
    pub(crate) fn depart_drivers(&mut self, now: Tick) -> Vec<SimulationVehicle> {
        let now = self.clock.tick_to_time(now);
        let mut vehicles = Vec::new();
        for mut driver in self.waiting_drivers.pop(now) {
            driver.advance_plan(now);
            let vehicle_id = driver.transit_driver().unwrap().run().vehicle.clone();
            {
                let mut events = self.comp_env.events_manager_borrow_mut();
                driver
                    .transit_driver()
                    .unwrap()
                    .emit_starts(now, &mut events);
                let leg = driver.curr_leg();
                events.process_event(
                    &PersonDepartureEventBuilder::default()
                        .time(now)
                        .person(driver.id().clone())
                        .link(driver.curr_link_id().unwrap().clone())
                        .leg_mode(leg.mode.clone())
                        .routing_mode(leg.routing_mode.clone().unwrap())
                        .build()
                        .unwrap(),
                );
                events.process_event(
                    &PersonEntersVehicleEventBuilder::default()
                        .time(now)
                        .person(driver.id().clone())
                        .vehicle(vehicle_id.clone())
                        .build()
                        .unwrap(),
                );
            }
            vehicles.push(self.garage.unpark_veh(driver, vehicle_id));
        }
        vehicles
    }

    /// Takes back a driver whose vehicle has arrived; its arrival is published already.
    pub(crate) fn receive_driver(&mut self, now: Tick, mut driver: SimulationAgent) {
        let now = self.clock.tick_to_time(now);
        driver.advance_plan(now);
        if driver.transit_driver().unwrap().is_finished() {
            self.finished_drivers.push(driver);
        } else {
            self.wait_for_departure(driver, now);
        }
    }

    /// Puts a passenger at the access stop of their PT leg. MATSim's `handleAgentPTDeparture`.
    pub(crate) fn receive_passenger(
        &mut self,
        now: Tick,
        agent: SimulationAgent,
        stops: &mut TransitStops,
    ) {
        let now = self.clock.tick_to_time(now);
        let leg = agent.curr_leg();
        let description = &leg
            .route
            .as_ref()
            .and_then(|route| route.as_pt())
            .unwrap_or_else(|| {
                panic!(
                    "Person {} departs on transit mode {} without a transit route.",
                    agent.id(),
                    leg.mode
                )
            })
            .description;
        let person = agent.id();
        let access = Id::<TransitStopFacility>::try_get_from_ext(&description.access_facility_id)
            .filter(|id| self.schedule.facilities().contains_key(id))
            .unwrap_or_else(|| unknown(person, "access stop", &description.access_facility_id));
        let egress = Id::<TransitStopFacility>::try_get_from_ext(&description.egress_facility_id)
            .filter(|id| self.schedule.facilities().contains_key(id))
            .unwrap_or_else(|| unknown(person, "egress stop", &description.egress_facility_id));
        let line = Id::<TransitLine>::try_get_from_ext(&description.transit_line_id)
            .filter(|id| self.schedule.lines().contains_key(id))
            .unwrap_or_else(|| unknown(person, "line", &description.transit_line_id));
        let stop_link = self.schedule.get_facility(&access).link_ref_id.as_ref();
        assert!(
            stop_link.is_none() || stop_link == agent.curr_link_id(),
            "Person {} tries to enter transit stop {} on link {} but is on link {}. {:?}",
            agent.id(),
            access,
            stop_link.unwrap(),
            agent.curr_link_id().unwrap(),
            agent.curr_leg()
        );

        self.comp_env.events_manager_borrow_mut().process_event(
            &AgentWaitingForPtEventBuilder::default()
                .time(now)
                .person(agent.id().clone())
                .at_stop(access.clone())
                .destination_stop(egress.clone())
                .build()
                .unwrap(),
        );
        stops.add(access, WaitingPassenger::new(agent, now, line, egress));
    }
}

fn unknown(person: &Id<InternalPerson>, kind: &str, id: &str) -> ! {
    panic!(
        "Person {person} plans a transit ride with {kind} {id}, which the schedule does not contain."
    )
}
