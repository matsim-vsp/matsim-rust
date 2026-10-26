use crate::simulation::Identifiable;
use crate::simulation::agents::SimulationAgentLogic;
use crate::simulation::agents::agent::SimulationAgent;
use crate::simulation::agents::{AgentEvent, EndTime, EnvironmentalEventObserver};
use crate::simulation::controller::ThreadLocalComputationalEnvironment;
use crate::simulation::engines::{
    emit_partition_enter_events_for_agent, emit_partition_leave_events_for_agent,
};
use crate::simulation::events::{
    AgentWaitingForPtEventBuilder, PersonArrivalEventBuilder, PersonDepartureEventBuilder,
    PersonEntersVehicleEventBuilder, PersonLeavesVehicleEventBuilder,
    TransitDriverStartsEventBuilder, VehicleArrivesAtFacilityEventBuilder,
    VehicleDepartsAtFacilityEventBuilder,
};
use crate::simulation::id::Id;
use crate::simulation::messaging::messages::TransitVehicleMessage;
use crate::simulation::messaging::partition_change::{
    PartitionChangeContext, PartitionChangeEntity,
};
use crate::simulation::pt::driver::{TransitDriver, TransitDriverLogic};
use crate::simulation::pt::schedule::{DepartureIndex, TransitSimSchedule};
use crate::simulation::scenario::transit::TransitLine;
use crate::simulation::scenario::vehicles::Garage;
use crate::simulation::time::{SimClock, SimTime, Tick};
use crate::simulation::time_queue::TimeQueue;
use crate::simulation::vehicles::SimulationVehicle;
use std::sync::Arc;

/// Moves the vehicles of schedule-based transit from stop to stop, without the network, like the
/// deterministic transit engine of SBB in MATSim. Each vehicle has a [`TransitDriver`], which
/// knows where the vehicle is and decides when it arrives and departs. Passengers wait at stops
/// and decide via [`TransitPassengerLogic`](super::passenger::TransitPassengerLogic) whether they
/// board and alight. The engine only moves vehicles and agents and emits the events.
///
/// The engine only does work for events, never per tick and vehicle: due starts, arrivals and
/// departures come from queues, and boarding is checked when a vehicle or a passenger arrives at a
/// stop. Simultaneous events are ordered by the departure index of the schedule.
///
/// Deviations from SBB: a vehicle needs at least one tick from one stop to the next, so that the
/// result does not depend on whether the next stop is on another partition; boarding and alighting
/// take no time, and vehicles have no capacity.
pub(crate) struct TransitEngine {
    schedule: Arc<TransitSimSchedule>,
    garage: Arc<Garage>,
    rank: u32,
    comp_env: ThreadLocalComputationalEnvironment,
    clock: SimClock,
    /// Departures whose first stop is on this partition, sorted by time and departure index.
    starts: Vec<DepartureIndex>,
    next_start: usize,
    /// Vehicles approaching their next stop.
    moving: TimeQueue<MovingVehicle, ()>,
    /// Vehicles halting at a stop, by when they depart.
    departures: TimeQueue<DueDeparture, ()>,
    /// Indexed by the internal id of the stop facility.
    stops: Vec<StopState>,
    /// Agents which reached their stop in this tick.
    arriving_passengers: Vec<SimulationAgent>,
    /// Vehicles leaving to other partitions, with the partition of their next stop.
    outgoing: Vec<(u32, TransitVehicleMessage)>,
}

/// A vehicle on its way, due when its driver arrives at the next stop.
struct MovingVehicle(SimulationVehicle);

impl EndTime for MovingVehicle {
    fn end_time(&self, now: SimTime) -> SimTime {
        self.0.driver().end_time(now)
    }
}

/// A halting vehicle is due to depart. The vehicle itself halts in [`StopState::halting`].
struct DueDeparture {
    stop: usize,
    departure: DepartureIndex,
    time: SimTime,
}

impl EndTime for DueDeparture {
    fn end_time(&self, _now: SimTime) -> SimTime {
        self.time
    }
}

#[derive(Default)]
struct StopState {
    /// In order of arrival, like in MATSim.
    waiting: Vec<WaitingPassenger>,
    /// Vehicles halting at the stop, in order of arrival.
    halting: Vec<SimulationVehicle>,
}

struct WaitingPassenger {
    line: Option<Id<TransitLine>>,
    agent: SimulationAgent,
}

fn transit_driver(vehicle: &SimulationVehicle) -> &dyn TransitDriver {
    vehicle
        .driver()
        .as_transit_driver()
        .unwrap_or_else(|| panic!("Transit vehicle {} has no transit driver.", vehicle.id()))
}

impl TransitEngine {
    pub(crate) fn new(
        schedule: Arc<TransitSimSchedule>,
        garage: Arc<Garage>,
        rank: u32,
        comp_env: ThreadLocalComputationalEnvironment,
        clock: SimClock,
    ) -> Self {
        let mut starts: Vec<DepartureIndex> = schedule
            .indexed_departures()
            .filter(|(_, departure)| schedule.route(departure.route).stops[0].partition == rank)
            .map(|(index, _)| index)
            .collect();
        starts.sort_by_key(|&index| (schedule.departure(index).time, index));

        let num_stops = schedule
            .routes()
            .iter()
            .flat_map(|route| &route.stops)
            .map(|stop| stop.facility.internal() as usize + 1)
            .max()
            .unwrap_or_default();
        let stops = std::iter::repeat_with(StopState::default)
            .take(num_stops)
            .collect();

        Self {
            schedule,
            garage,
            rank,
            comp_env,
            clock,
            starts,
            next_start: 0,
            moving: TimeQueue::new(),
            departures: TimeQueue::new(),
            stops,
            arriving_passengers: Vec::new(),
            outgoing: Vec::new(),
        }
    }

    /// An agent starts a transit leg. It waits at its stop from this tick on.
    pub(crate) fn receive_agent(&mut self, agent: SimulationAgent) {
        self.arriving_passengers.push(agent);
    }

    /// A vehicle arrives from another partition. Its driver knows when it reaches its next stop.
    pub(crate) fn receive_remote_vehicle(
        &mut self,
        now: Tick,
        vehicle: SimulationVehicle,
        from: u32,
    ) {
        let now_time = self.clock.tick_to_time(now);
        let due_tick = self.clock.time_to_tick(vehicle.driver().end_time(now_time));
        assert!(
            now <= due_tick,
            "Transit vehicle {} from partition {from} to partition {} arrived at tick {} after it was due at its next stop at tick {}. \
            This might happen if the partitions share no link. To mitigate this problem, you might enable the global sync.",
            vehicle.id(),
            self.rank,
            now.value(),
            due_tick.value(),
        );
        for passenger in vehicle.passengers() {
            emit_partition_enter_events_for_agent(&mut self.comp_env, passenger, from, now_time);
        }
        let order = transit_driver(&vehicle).departure_index().order();
        self.moving
            .add_with_order(MovingVehicle(vehicle), now_time, order);
    }

    /// Vehicles which left for other partitions in the last step, with the partition of their
    /// next stop.
    pub(crate) fn take_outgoing(&mut self) -> Vec<(u32, TransitVehicleMessage)> {
        std::mem::take(&mut self.outgoing)
    }

    /// Performs one step and returns the passengers who alighted at the end of their leg.
    #[hotpath::measure]
    pub(crate) fn do_step(&mut self, now: Tick) -> Vec<SimulationAgent> {
        let now_time = self.clock.tick_to_time(now);
        let mut alighted = Vec::new();

        self.start_departures(now_time);
        for MovingVehicle(vehicle) in self.moving.pop(now_time) {
            self.arrive(vehicle, now_time, &mut alighted);
        }
        for agent in std::mem::take(&mut self.arriving_passengers) {
            self.start_waiting(agent, now_time);
        }
        for due in self.departures.pop(now_time) {
            self.depart(due, now_time, now);
        }
        alighted
    }

    /// Returns all agents which are still waiting or riding, e.g. at the end of the simulation.
    /// Drivers are dropped; they are no persons of the population.
    pub(crate) fn drain(&mut self) -> Vec<SimulationAgent> {
        let mut result = Vec::new();
        for stop in &mut self.stops {
            result.extend(
                std::mem::take(&mut stop.waiting)
                    .into_iter()
                    .map(|w| w.agent),
            );
            for vehicle in std::mem::take(&mut stop.halting) {
                result.extend(vehicle.into_passengers());
            }
        }
        for MovingVehicle(vehicle) in self.moving.drain() {
            result.extend(vehicle.into_passengers());
        }
        self.departures.drain();
        result.extend(std::mem::take(&mut self.arriving_passengers));
        result
    }

    fn start_departures(&mut self, now: SimTime) {
        while let Some(&index) = self.starts.get(self.next_start) {
            let departure = self.schedule.departure(index);
            if departure.time > now {
                break;
            }
            self.next_start += 1;

            let route = self.schedule.route(departure.route);
            let vehicle = self
                .garage
                .vehicles
                .get(&departure.vehicle)
                .unwrap_or_else(|| {
                    panic!(
                        "Transit vehicle {} is not in the transit garage.",
                        departure.vehicle
                    )
                });
            let mut events = self.comp_env.events_manager_borrow_mut();
            events.process_event(
                &TransitDriverStartsEventBuilder::default()
                    .time(now)
                    .driver(departure.driver.clone())
                    .vehicle(departure.vehicle.clone())
                    .transit_line(route.line.clone())
                    .transit_route(route.route.clone())
                    .departure(departure.id.clone())
                    .build()
                    .unwrap(),
            );
            events.process_event(
                &PersonDepartureEventBuilder::default()
                    .time(now)
                    .person(departure.driver.clone())
                    .link(route.start_link.clone())
                    .leg_mode(route.mode.clone())
                    .build()
                    .unwrap(),
            );
            events.process_event(
                &PersonEntersVehicleEventBuilder::default()
                    .time(now)
                    .person(departure.driver.clone())
                    .vehicle(departure.vehicle.clone())
                    .build()
                    .unwrap(),
            );
            drop(events);

            let driver = SimulationAgent::new(Box::new(TransitDriverLogic::new(
                self.schedule.clone(),
                index,
                now,
            )));
            let vehicle = SimulationVehicle::new(vehicle.clone(), Some(driver), Vec::new());
            self.moving
                .add_with_order(MovingVehicle(vehicle), now, index.order());
        }
    }

    fn arrive(
        &mut self,
        mut vehicle: SimulationVehicle,
        now: SimTime,
        alighted: &mut Vec<SimulationAgent>,
    ) {
        let driver = transit_driver(&vehicle);
        let departure = driver.departure_index();
        let facility = driver.current_stop().facility.clone();
        let vehicle_id = vehicle.id().clone();

        self.comp_env.events_manager_borrow_mut().process_event(
            &VehicleArrivesAtFacilityEventBuilder::default()
                .time(now)
                .vehicle(vehicle_id.clone())
                .facility(facility.clone())
                .build()
                .unwrap(),
        );
        vehicle
            .driver_mut()
            .notify_event(&mut AgentEvent::TransitStopArrived(), now);

        let leaving = vehicle.take_passengers_where(|passenger| {
            passenger
                .as_transit_passenger()
                .is_some_and(|passenger| passenger.wants_to_alight(&facility))
        });
        for mut passenger in leaving {
            self.comp_env.events_manager_borrow_mut().process_event(
                &PersonLeavesVehicleEventBuilder::default()
                    .time(now)
                    .person(passenger.id().clone())
                    .vehicle(vehicle_id.clone())
                    .build()
                    .unwrap(),
            );
            passenger.notify_event(&mut AgentEvent::TransitVehicleLeft(), now);
            alighted.push(passenger);
        }

        let driver = transit_driver(&vehicle);
        let route = driver.route();
        let upcoming = driver.upcoming_stops();
        let stop = facility.internal() as usize;
        let state = &mut self.stops[stop];
        // Collected first, because the driver's route and stops borrow the vehicle.
        let mut boarding = Vec::new();
        if !upcoming.is_empty() && !state.waiting.is_empty() {
            for waiting in std::mem::take(&mut state.waiting) {
                let boards = waiting.line.as_ref().is_none_or(|line| *line == route.line)
                    && waiting
                        .agent
                        .as_transit_passenger()
                        .is_some_and(|p| p.wants_to_board(&route.line, &route.route, upcoming));
                if boards {
                    self.comp_env.events_manager_borrow_mut().process_event(
                        &PersonEntersVehicleEventBuilder::default()
                            .time(now)
                            .person(waiting.agent.id().clone())
                            .vehicle(vehicle_id.clone())
                            .build()
                            .unwrap(),
                    );
                    boarding.push(waiting.agent);
                } else {
                    state.waiting.push(waiting);
                }
            }
        }
        for agent in boarding {
            vehicle.add_passenger(agent);
        }

        let due = DueDeparture {
            stop,
            departure,
            time: vehicle.driver().end_time(now),
        };
        state.halting.push(vehicle);
        self.departures.add_with_order(due, now, departure.order());
    }

    fn start_waiting(&mut self, agent: SimulationAgent, now: SimTime) {
        let passenger = agent.as_transit_passenger().unwrap_or_else(|| {
            panic!(
                "Agent {} starts a transit leg without a transit route.",
                agent.id()
            )
        });
        let access = passenger.access_stop();
        let line = passenger.waiting_for_line();
        self.comp_env.events_manager_borrow_mut().process_event(
            &AgentWaitingForPtEventBuilder::default()
                .time(now)
                .agent(agent.id().clone())
                .at_stop(access.clone())
                .destination_stop(passenger.egress_stop())
                .build()
                .unwrap(),
        );

        let state = self
            .stops
            .get_mut(access.internal() as usize)
            .unwrap_or_else(|| {
                panic!(
                    "Agent {} waits at stop {access}, which no transit route serves.",
                    agent.id()
                )
            });
        // Board the first halting vehicle which the agent wants to take. This replaces the check
        // of SBB's engine in every second while a vehicle halts.
        let boarding = state.halting.iter().position(|vehicle| {
            let driver = transit_driver(vehicle);
            let route = driver.route();
            let upcoming = driver.upcoming_stops();
            !upcoming.is_empty()
                && line.as_ref().is_none_or(|line| *line == route.line)
                && passenger.wants_to_board(&route.line, &route.route, upcoming)
        });

        match boarding {
            Some(position) => {
                let vehicle = &mut state.halting[position];
                self.comp_env.events_manager_borrow_mut().process_event(
                    &PersonEntersVehicleEventBuilder::default()
                        .time(now)
                        .person(agent.id().clone())
                        .vehicle(vehicle.id().clone())
                        .build()
                        .unwrap(),
                );
                vehicle.add_passenger(agent);
            }
            None => state.waiting.push(WaitingPassenger { line, agent }),
        }
    }

    fn depart(&mut self, due: DueDeparture, now: SimTime, now_tick: Tick) {
        let halting = &mut self.stops[due.stop].halting;
        let position_in_stop = halting
            .iter()
            .position(|vehicle| transit_driver(vehicle).departure_index() == due.departure)
            .expect("A departing vehicle halts at its stop.");
        let mut vehicle = halting.remove(position_in_stop);
        let driver = transit_driver(&vehicle);
        let departure = driver.departure_index();
        let at_last_stop = driver.is_at_last_stop();

        self.comp_env.events_manager_borrow_mut().process_event(
            &VehicleDepartsAtFacilityEventBuilder::default()
                .time(now)
                .vehicle(vehicle.id().clone())
                .facility(driver.current_stop().facility.clone())
                .build()
                .unwrap(),
        );

        if at_last_stop {
            self.finish_departure(vehicle, now);
            return;
        }

        let earliest_arrival = now + SimTime::from_duration(self.clock.tick_length());
        vehicle.driver_mut().notify_event(
            &mut AgentEvent::TransitStopDeparted { earliest_arrival },
            now,
        );
        // The driver now approaches the next stop.
        let to = transit_driver(&vehicle).current_stop().partition;

        if to == self.rank {
            self.moving
                .add_with_order(MovingVehicle(vehicle), now, departure.order());
        } else {
            for passenger in vehicle.passengers() {
                emit_partition_leave_events_for_agent(&mut self.comp_env, passenger, to, now);
            }
            let context = PartitionChangeContext {
                time: now,
                from: self.rank,
                to,
            };
            let attachments = self
                .comp_env
                .partition_migration_extensions_manager_borrow_mut()
                .send(PartitionChangeEntity::TransitVehicle(&vehicle), &context);
            debug_assert!(self.clock.time_to_tick(vehicle.driver().end_time(now)) > now_tick);
            self.outgoing
                .push((to, TransitVehicleMessage::new(vehicle, attachments)));
        }
    }

    /// The vehicle left its last stop: the driver leaves the vehicle and ends its leg.
    fn finish_departure(&mut self, vehicle: SimulationVehicle, now: SimTime) {
        let driver = transit_driver(&vehicle);
        assert!(
            vehicle.passengers().is_empty(),
            "Passengers {:?} are still in transit vehicle {} at the last stop of route {} of line {}.",
            vehicle
                .passengers()
                .iter()
                .map(|passenger| passenger.id().external())
                .collect::<Vec<_>>(),
            vehicle.id(),
            driver.route().route,
            driver.route().line
        );

        let mut events = self.comp_env.events_manager_borrow_mut();
        events.process_event(
            &PersonLeavesVehicleEventBuilder::default()
                .time(now)
                .person(driver.departure().driver.clone())
                .vehicle(vehicle.id().clone())
                .build()
                .unwrap(),
        );
        events.process_event(
            &PersonArrivalEventBuilder::default()
                .time(now)
                .person(driver.departure().driver.clone())
                .link(driver.route().end_link.clone())
                .leg_mode(driver.route().mode.clone())
                .build()
                .unwrap(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::TransitEngine;
    use crate::simulation::Identifiable;
    use crate::simulation::agents::EndTime;
    use crate::simulation::agents::SimulationAgentLogic;
    use crate::simulation::agents::agent::SimulationAgent;
    use crate::simulation::controller::ThreadLocalComputationalEnvironment;
    use crate::simulation::id::Id;
    use crate::simulation::io::xml::events::XmlEventsWriter;
    use crate::simulation::pt::schedule::TransitSimSchedule;
    use crate::simulation::scenario::network::{Link, Network};
    use crate::simulation::scenario::population::Population;
    use crate::simulation::scenario::transit::TransitSchedule;
    use crate::simulation::scenario::vehicles::Garage;
    use crate::simulation::time::{SimClock, SimTime, Tick};
    use crate::test_utils::create_pt_agent;
    use macros::deterministic_id_test;
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};
    use std::rc::Rc;
    use std::sync::Arc;

    const NETWORK: &str = "./assets/pt_tutorial/multimodalnetwork.xml";
    const SCHEDULE: &str = "./assets/pt_tutorial/transitschedule.xml";
    const VEHICLES: &str = "./assets/pt_tutorial/transitVehicles.xml";
    const SIX: u64 = 6 * 3600;

    struct Setup {
        schedule: Arc<TransitSimSchedule>,
        garage: Arc<Garage>,
    }

    impl Setup {
        fn tutorial() -> Self {
            Self::from_path(Path::new(SCHEDULE), &[])
        }

        /// One line `L` on the tutorial network with stop `s1` on link 11, `s2` on link 12 and
        /// `s3` on link 33.
        fn small(routes: &str, partition_of_link_12: u32) -> Self {
            let xml = format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE transitSchedule SYSTEM "http://www.matsim.org/files/dtd/transitSchedule_v1.dtd">
<transitSchedule>
    <transitStops>
        <stopFacility id="s1" x="0" y="0" linkRefId="11"/>
        <stopFacility id="s2" x="0" y="0" linkRefId="12"/>
        <stopFacility id="s3" x="0" y="0" linkRefId="33"/>
    </transitStops>
    <transitLine id="L">{routes}</transitLine>
</transitSchedule>"#
            );
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("schedule.xml");
            std::fs::write(&path, xml).unwrap();
            Self::from_path(&path, &[("12", partition_of_link_12)])
        }

        fn from_path(path: &Path, partitions: &[(&str, u32)]) -> Self {
            let mut network = Network::from_file(NETWORK);
            for (link, partition) in partitions {
                network
                    .get_link_mut(&Id::<Link>::get_from_ext(link))
                    .partition = *partition;
            }
            let schedule = TransitSchedule::from_file(path);
            let garage = Garage::from_file(&PathBuf::from(VEHICLES));
            let schedule =
                TransitSimSchedule::build(&schedule, &network, &garage, &Population::default());
            Self {
                schedule: Arc::new(schedule),
                garage: Arc::new(garage),
            }
        }

        fn engine(&self, rank: u32) -> (TransitEngine, Rc<RefCell<Vec<String>>>) {
            let mut comp_env = ThreadLocalComputationalEnvironment::default();
            let events = Rc::new(RefCell::new(Vec::new()));
            let log = events.clone();
            comp_env.events_manager_borrow_mut().on_any(move |event| {
                log.borrow_mut()
                    .push(XmlEventsWriter::event_2_string(event).trim().to_string())
            });
            let engine = TransitEngine::new(
                self.schedule.clone(),
                self.garage.clone(),
                rank,
                comp_env,
                SimClock::new(1),
            );
            (engine, events)
        }
    }

    const SMALL_ROUTE: &str = r#"
        <transitRoute id="R">
            <transportMode>train</transportMode>
            <routeProfile>
                <stop refId="s1" departureOffset="00:00:00"/>
                <stop refId="s2" arrivalOffset="00:01:00" departureOffset="00:01:30"/>
                <stop refId="s3" arrivalOffset="00:03:00"/>
            </routeProfile>
            <route><link refId="11"/><link refId="12"/><link refId="23"/><link refId="33"/></route>
            <departures>
                <departure id="1" departureTime="06:00:00" vehicleRefId="tr_1"/>
            </departures>
        </transitRoute>"#;

    /// Steps the engine from `from` to `to` (inclusive) and returns the alighted agents with the
    /// tick at which they alighted.
    fn run(engine: &mut TransitEngine, from: u64, to: u64) -> Vec<(u64, SimulationAgent)> {
        (from..=to)
            .flat_map(|tick| {
                engine
                    .do_step(Tick::new(tick))
                    .into_iter()
                    .map(move |agent| (tick, agent))
            })
            .collect()
    }

    fn tutorial_agent(id: u64) -> SimulationAgent {
        create_pt_agent(id, "Blue Line", "1to3", "1", "3", "11", "33")
    }

    fn events_of(events: &Rc<RefCell<Vec<String>>>, needle: &str) -> Vec<String> {
        events
            .borrow()
            .iter()
            .filter(|event| event.contains(needle))
            .cloned()
            .collect()
    }

    #[deterministic_id_test]
    fn early_agent_boards_first_vehicle_and_alights_at_egress_stop() {
        let setup = Setup::tutorial();
        let (mut engine, events) = setup.engine(0);

        engine.receive_agent(tutorial_agent(1));
        let alighted = run(&mut engine, SIX - 100, SIX + 600);

        assert_eq!(1, alighted.len());
        let (tick, agent) = &alighted[0];
        assert_eq!(SIX + 540, *tick);
        assert_eq!(Some(&Id::<Link>::get_from_ext("33")), agent.curr_link_id());
        assert_eq!(
            vec![
                format!(
                    r#"<event time="{}" type="waitingForPt" agent="1" atStop="1" destinationStop="3"/>"#,
                    SIX - 100
                ),
                format!(
                    r#"<event time="{SIX}" type="PersonEntersVehicle" person="1" vehicle="tr_1"/>"#
                ),
                format!(
                    r#"<event time="{}" type="PersonLeavesVehicle" person="1" vehicle="tr_1"/>"#,
                    SIX + 540
                ),
            ],
            events_of(&events, r#"person="1""#)
                .into_iter()
                .chain(events_of(&events, r#"agent="1""#))
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>()
        );
    }

    #[deterministic_id_test]
    fn driver_starts_halts_at_every_stop_and_ends_at_last_stop() {
        let setup = Setup::small(SMALL_ROUTE, 0);
        let (mut engine, events) = setup.engine(0);

        run(&mut engine, SIX, SIX + 200);

        let expected = [
            (SIX, r#"type="TransitDriverStarts" driverId="pt_L_R_1" vehicleId="tr_1" transitLineId="L" transitRouteId="R" departureId="1""#),
            (SIX, r#"type="departure" person="pt_L_R_1" link="11" legMode="train""#),
            (SIX, r#"type="PersonEntersVehicle" person="pt_L_R_1" vehicle="tr_1""#),
            (SIX, r#"type="VehicleArrivesAtFacility" vehicle="tr_1" facility="s1" delay="0""#),
            (SIX, r#"type="VehicleDepartsAtFacility" vehicle="tr_1" facility="s1" delay="0""#),
            (SIX + 60, r#"type="VehicleArrivesAtFacility" vehicle="tr_1" facility="s2" delay="0""#),
            (SIX + 90, r#"type="VehicleDepartsAtFacility" vehicle="tr_1" facility="s2" delay="0""#),
            (SIX + 180, r#"type="VehicleArrivesAtFacility" vehicle="tr_1" facility="s3" delay="0""#),
            (SIX + 180, r#"type="VehicleDepartsAtFacility" vehicle="tr_1" facility="s3" delay="0""#),
            (SIX + 180, r#"type="PersonLeavesVehicle" person="pt_L_R_1" vehicle="tr_1""#),
            (SIX + 180, r#"type="arrival" person="pt_L_R_1" link="33" legMode="train""#),
        ]
        .map(|(time, rest)| format!(r#"<event time="{time}" {rest}/>"#));
        assert_eq!(expected.to_vec(), *events.borrow());
    }

    #[deterministic_id_test]
    fn agent_missing_departure_takes_next_vehicle_of_line_but_not_one_ending_at_its_stop() {
        let setup = Setup::tutorial();
        let (mut engine, events) = setup.engine(0);

        // The 06:00 vehicle has left. The vehicle of route 3to1 ends at stop 1 at 06:09, so it
        // does not serve stop 3 any more. The agent takes the 06:15 vehicle.
        let alighted = {
            run(&mut engine, SIX, SIX);
            engine.receive_agent(tutorial_agent(1));
            run(&mut engine, SIX + 1, SIX + 1500)
        };

        assert_eq!(SIX + 900 + 540, alighted[0].0);
        assert_eq!(
            vec![format!(
                r#"<event time="{}" type="PersonEntersVehicle" person="1" vehicle="tr_2"/>"#,
                SIX + 900
            )],
            events_of(&events, r#"PersonEntersVehicle" person="1""#)
        );
    }

    #[deterministic_id_test]
    fn agent_arriving_while_vehicle_halts_boards_immediately() {
        let setup = Setup::small(SMALL_ROUTE, 0);
        let (mut engine, events) = setup.engine(0);

        run(&mut engine, SIX, SIX + 70);
        // The vehicle halts at s2 from 06:01:00 to 06:01:30.
        engine.receive_agent(create_pt_agent(1, "L", "R", "s2", "s3", "12", "33"));
        let alighted = run(&mut engine, SIX + 71, SIX + 200);

        assert_eq!(
            vec![SIX + 180],
            alighted.iter().map(|(t, _)| *t).collect::<Vec<_>>()
        );
        assert_eq!(
            vec![
                format!(
                    r#"<event time="{}" type="PersonEntersVehicle" person="1" vehicle="tr_1"/>"#,
                    SIX + 71
                ),
                format!(
                    r#"<event time="{}" type="PersonLeavesVehicle" person="1" vehicle="tr_1"/>"#,
                    SIX + 180
                ),
            ],
            events_of(&events, r#"person="1""#)
        );
    }

    #[deterministic_id_test]
    fn simultaneous_vehicles_are_handled_in_departure_order() {
        let routes = r#"
            <transitRoute id="B">
                <transportMode>train</transportMode>
                <routeProfile>
                    <stop refId="s1" departureOffset="00:00:00"/>
                    <stop refId="s2" arrivalOffset="00:01:00"/>
                </routeProfile>
                <route><link refId="11"/><link refId="12"/></route>
                <departures><departure id="1" departureTime="06:00:00" vehicleRefId="tr_2"/></departures>
            </transitRoute>
            <transitRoute id="A">
                <transportMode>train</transportMode>
                <routeProfile>
                    <stop refId="s1" departureOffset="00:00:00"/>
                    <stop refId="s2" arrivalOffset="00:01:00"/>
                </routeProfile>
                <route><link refId="11"/><link refId="12"/></route>
                <departures><departure id="1" departureTime="06:00:00" vehicleRefId="tr_1"/></departures>
            </transitRoute>"#;
        let setup = Setup::small(routes, 0);
        let (mut engine, events) = setup.engine(0);

        engine.receive_agent(create_pt_agent(1, "L", "B", "s1", "s2", "11", "12"));
        run(&mut engine, SIX - 1, SIX + 60);

        // Route A comes first by its driver id, regardless of the order in the file. The agent
        // takes the first vehicle of its line, even if it planned route B.
        assert_eq!(
            vec!["pt_L_A_1", "pt_L_B_1"],
            events_of(&events, "TransitDriverStarts")
                .iter()
                .map(|e| e.split('"').nth(5).unwrap().to_string())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            vec![format!(
                r#"<event time="{SIX}" type="PersonEntersVehicle" person="1" vehicle="tr_1"/>"#
            )],
            events_of(&events, r#"PersonEntersVehicle" person="1""#)
        );
    }

    #[deterministic_id_test]
    fn vehicle_needs_at_least_one_tick_between_stops() {
        let routes = r#"
            <transitRoute id="R">
                <transportMode>train</transportMode>
                <routeProfile>
                    <stop refId="s1" departureOffset="00:00:00"/>
                    <stop refId="s2" arrivalOffset="00:00:00"/>
                </routeProfile>
                <route><link refId="11"/><link refId="12"/></route>
                <departures><departure id="1" departureTime="06:00:00" vehicleRefId="tr_1"/></departures>
            </transitRoute>"#;
        let setup = Setup::small(routes, 0);
        let (mut engine, events) = setup.engine(0);

        run(&mut engine, SIX, SIX + 2);

        assert_eq!(
            vec![
                format!(
                    r#"<event time="{SIX}" type="VehicleArrivesAtFacility" vehicle="tr_1" facility="s1" delay="0"/>"#
                ),
                format!(
                    r#"<event time="{}" type="VehicleArrivesAtFacility" vehicle="tr_1" facility="s2" delay="0"/>"#,
                    SIX + 1
                ),
            ],
            events_of(&events, "VehicleArrivesAtFacility")
        );
    }

    #[deterministic_id_test]
    fn drain_returns_waiting_and_riding_agents() {
        let setup = Setup::small(SMALL_ROUTE, 0);
        let (mut engine, _) = setup.engine(0);

        engine.receive_agent(create_pt_agent(1, "L", "R", "s1", "s3", "11", "33"));
        engine.receive_agent(create_pt_agent(2, "L", "R", "s3", "s1", "33", "11"));
        run(&mut engine, SIX, SIX + 30);

        let mut ids: Vec<_> = engine
            .drain()
            .iter()
            .map(|agent| agent.id().external().to_string())
            .collect();
        ids.sort();
        assert_eq!(vec!["1", "2"], ids);
        assert!(engine.drain().is_empty());
    }

    #[deterministic_id_test]
    fn vehicle_moves_to_partition_of_next_stop_with_its_passengers() {
        let setup = Setup::small(SMALL_ROUTE, 1);
        let (mut first, first_events) = setup.engine(0);
        let (mut second, second_events) = setup.engine(1);

        first.receive_agent(create_pt_agent(1, "L", "R", "s1", "s2", "11", "12"));
        run(&mut first, SIX, SIX);
        let mut outgoing = first.take_outgoing();
        assert_eq!(1, outgoing.len());
        let (to, message) = outgoing.pop().unwrap();
        assert_eq!(1, to);
        let (vehicle, _) = message.into_parts();
        let arrival = vehicle.driver().end_time(SimTime::default());
        assert_eq!(
            "s2",
            super::transit_driver(&vehicle)
                .current_stop()
                .facility
                .external()
        );
        assert_eq!(1, vehicle.passengers().len());
        assert_eq!(SIX + 60, arrival.as_secs());

        second.receive_remote_vehicle(Tick::new(SIX + 1), vehicle, 0);
        let alighted = run(&mut second, SIX + 1, SIX + 100);

        assert_eq!(SIX + 60, alighted[0].0);
        assert!(events_of(&first_events, "facility=\"s2\"").is_empty());
        assert_eq!(2, events_of(&second_events, "facility=\"s2\"").len());
        // Stop s3 is on partition 0 again.
        assert_eq!(1, second.take_outgoing().len());
    }

    #[deterministic_id_test]
    #[should_panic(expected = "after it was due at its next stop")]
    fn late_remote_vehicle_panics() {
        let setup = Setup::small(SMALL_ROUTE, 1);
        let (mut first, _) = setup.engine(0);
        let (mut second, _) = setup.engine(1);

        run(&mut first, SIX, SIX);
        let (_, message) = first.take_outgoing().pop().unwrap();
        let (vehicle, _) = message.into_parts();
        second.receive_remote_vehicle(Tick::new(SIX + 61), vehicle, 0);
    }
}
