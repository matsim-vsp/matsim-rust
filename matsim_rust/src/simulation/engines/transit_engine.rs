use crate::simulation::agents::{EndTime, LegAgent, PassengerAgent, TransitDriverAgent};
use crate::simulation::controller::ThreadLocalComputationalEnvironment;
use crate::simulation::engines::{
    emit_partition_enter_events_for_agent, emit_partition_leave_events_for_agent,
};
use crate::simulation::events::{
    AgentWaitingForPtEventBuilder, PersonDepartureEventBuilder, PersonEntersVehicleEventBuilder,
    PersonLeavesVehicleEventBuilder, TransitDriverStartsEventBuilder,
    VehicleArrivesAtFacilityEventBuilder, VehicleDepartsAtFacilityEventBuilder,
};
use crate::simulation::id::Id;
use crate::simulation::messaging::messages::TransitVehicleMessage;
use crate::simulation::messaging::partition_change::{
    PartitionChangeContext, PartitionChangeEntity,
};
use crate::simulation::pt::schedule::TransitSimSchedule;
use crate::simulation::queue::TimeQueue;
use crate::simulation::scenario::transit::TransitStopFacility;
use crate::simulation::scenario::vehicles::InternalVehicle;
use crate::simulation::time::{SimClock, SimTime, Tick};
use crate::simulation::vehicles::TransitVehicle;
use itertools::Itertools;
use nohash_hasher::IntMap;
use std::sync::Arc;
use std::time::Duration;

/// The time a passenger needs to alight. Alighting takes no time yet.
const ALIGHTING_TIME: Duration = Duration::ZERO;
/// The time a passenger needs to board. Boarding takes no time yet.
const BOARDING_TIME: Duration = Duration::ZERO;

/// Moves the vehicles of schedule-based transit from stop to stop without the network, like SBB's
/// deterministic transit engine in MATSim. Drivers decide when their vehicles arrive and depart,
/// passengers decide whether they board and alight. The engine carries that out and emits the
/// events.
pub(crate) struct TransitEngine {
    schedule: Arc<TransitSimSchedule>,
    rank: u32,
    comp_env: ThreadLocalComputationalEnvironment,
    clock: SimClock,
    approaching: TimeQueue<TransitVehicle, ()>,
    departing: TimeQueue<DepartureTracker, ()>,
    new_passengers: Vec<PassengerAgent>,
    stops: IntMap<Id<TransitStopFacility>, StopState>,
    /// The stops to process in this tick: alighting and boarding ongoing.
    stops_to_be_processed: Vec<Id<TransitStopFacility>>,
}

/// What leaves the transit engine in a step.
#[derive(Default)]
pub(crate) struct TransitStepResult {
    /// Passengers who alighted and drivers whose departure ended. They end their leg.
    pub(crate) arrived: Vec<LegAgent>,
    /// Vehicles that left for a stop on another partition, with that partition.
    pub(crate) leaving: Vec<(u32, TransitVehicleMessage)>,
}

struct HaltingVehicle {
    vehicle: TransitVehicle,
    doors: Doors,
    doors_used_until: SimTime,
}

#[derive(PartialEq)]
enum Doors {
    Alighting,
    Boarding { complete: bool },
}

/// The departure of `vehicle`, which halts at `stop`. Each departure has a vehicle of its own.
struct DepartureTracker {
    stop: Id<TransitStopFacility>,
    vehicle: Id<InternalVehicle>,
    time: SimTime,
}

#[derive(Default)]
struct StopState {
    /// In the order in which the passengers started to wait, within a tick by id.
    waiting_passengers: Vec<PassengerAgent>,
    /// In the order in which the vehicles arrived.
    halting_vehicles: Vec<HaltingVehicle>,
}

impl EndTime for TransitVehicle {
    fn end_time(&self, _now: SimTime) -> SimTime {
        self.driver().next_time()
    }
}

impl EndTime for DepartureTracker {
    fn end_time(&self, _now: SimTime) -> SimTime {
        self.time
    }
}

/// The order of a vehicle among simultaneous events. Driver ids are created in the order of the
/// departures and after all person ids, so this order is stable.
fn order(vehicle: &TransitVehicle) -> u64 {
    vehicle.driver().id().internal()
}

impl TransitEngine {
    pub(crate) fn new(
        schedule: Arc<TransitSimSchedule>,
        rank: u32,
        comp_env: ThreadLocalComputationalEnvironment,
        clock: SimClock,
    ) -> Self {
        // Create empty state for each stop.
        let stops = schedule
            .served_stops()
            .map(|stop| (stop.clone(), StopState::default()))
            .collect();

        Self {
            schedule,
            rank,
            comp_env,
            clock,
            approaching: TimeQueue::new(),
            departing: TimeQueue::new(),
            stops,
            new_passengers: Vec::new(),
            stops_to_be_processed: Vec::new(),
        }
    }

    /// An agent starts a transit leg. It waits at its stop from this tick on, so a vehicle that
    /// arrives or departs in this tick can take it.
    pub(crate) fn receive_agent(&mut self, passenger: PassengerAgent) {
        self.new_passengers.push(passenger);
    }

    /// A vehicle arrives from partition `from`. It is received after this tick's step, so its
    /// arrival at the next stop must be due in a later tick.
    pub(crate) fn receive_remote_vehicle(&mut self, now: Tick, vehicle: TransitVehicle, from: u32) {
        let due_tick = self.clock.time_to_tick(vehicle.driver().next_time());
        assert!(
            now < due_tick,
            "Transit vehicle {} from partition {from} to partition {} was received in tick {}, but it \
             is due at stop {} in tick {}. This might happen if the partitions share no link. To \
             mitigate this problem, you might enable the global sync.",
            vehicle.id(),
            self.rank,
            now.value(),
            vehicle.driver().stop(),
            due_tick.value(),
        );

        let now = self.clock.tick_to_time(now);
        for passenger in vehicle.passengers() {
            emit_partition_enter_events_for_agent(
                &mut self.comp_env,
                passenger.as_ref(),
                from,
                now,
            );
        }
        let order = order(&vehicle);
        self.approaching.add_with_order(vehicle, now, order);
    }

    /// Performs one step and returns the agents who arrived and the vehicles that left for another partition.
    #[hotpath::measure]
    pub(crate) fn do_step(&mut self, now: Tick) -> TransitStepResult {
        // The start of the next tick, computed like `now`, so that both round alike.
        let next = self.clock.tick_to_time(now.next());
        let now = self.clock.tick_to_time(now);
        let mut result = TransitStepResult::default();

        self.arrive(now);
        result.arrived = self.process_halting(now, next);
        self.depart(now, next, &mut result);
        result
    }

    /// Returns all agents that still wait or ride, e.g. at the end of the mobsim.
    pub(crate) fn drain(&mut self) -> Vec<LegAgent> {
        let mut agents = std::mem::take(&mut self.new_passengers);
        // Sorted, so that the order of the agents does not depend on the map.
        for (_, state) in self
            .stops
            .iter_mut()
            .sorted_unstable_by_key(|(stop, _)| *stop)
        {
            agents.append(&mut state.waiting_passengers);
            for mut halting in state.halting_vehicles.drain(..) {
                agents.extend(halting.vehicle.take_passengers());
            }
        }
        for mut vehicle in self.approaching.drain() {
            agents.extend(vehicle.take_passengers());
        }
        self.departing.drain();
        agents.into_iter().map(|agent| agent as LegAgent).collect()
    }

    /// A driver starts its departure. It enters its vehicle, which approaches the first stop. The
    /// engine emits the start of the departure, the departure of the driver's leg and its entering
    /// the vehicle, in this order as MATSim does.
    pub(crate) fn receive_driver(&mut self, now: Tick, driver: TransitDriverAgent) {
        let now = self.clock.tick_to_time(now);
        let mut events = self.comp_env.events_manager_borrow_mut();
        events.process_event(
            &TransitDriverStartsEventBuilder::default()
                .time(now)
                .driver(driver.id().clone())
                .vehicle(driver.vehicle().clone())
                .transit_line(driver.line().clone())
                .transit_route(driver.route().clone())
                .departure(driver.departure().clone())
                .build()
                .unwrap(),
        );
        events.process_event(
            &PersonDepartureEventBuilder::default()
                .time(now)
                .person(driver.id().clone())
                .link(driver.departure_link().clone())
                .leg_mode(driver.mode().clone())
                .build()
                .unwrap(),
        );
        events.process_event(
            &PersonEntersVehicleEventBuilder::default()
                .time(now)
                .person(driver.id().clone())
                .vehicle(driver.vehicle().clone())
                .build()
                .unwrap(),
        );
        drop(events);

        let vehicle = TransitVehicle::new(driver.vehicle().clone(), driver);
        let order = order(&vehicle);
        self.approaching.add_with_order(vehicle, now, order);
    }

    /// Vehicles reach their stops. Their passengers alight when the stop is processed.
    fn arrive(&mut self, now: SimTime) {
        for mut vehicle in self.approaching.pop(now) {
            let stop = vehicle.driver().stop().clone();
            self.comp_env.events_manager_borrow_mut().process_event(
                &VehicleArrivesAtFacilityEventBuilder::default()
                    .time(now)
                    .vehicle(vehicle.id().clone())
                    .facility(stop.clone())
                    .build()
                    .unwrap(),
            );
            vehicle.driver_mut().notify_stop_arrived(now);

            let due = DepartureTracker {
                stop: stop.clone(),
                vehicle: vehicle.id().clone(),
                time: vehicle.driver().next_time(),
            };
            self.departing.add_with_order(due, now, order(&vehicle));
            self.stops
                .get_mut(&stop)
                .expect("Vehicles only halt at stops that a route serves.")
                .halting_vehicles
                .push(HaltingVehicle {
                    vehicle,
                    doors: Doors::Alighting,
                    doors_used_until: now,
                });
            self.stops_to_be_processed.push(stop);
        }
    }

    /// Processes the stops where a vehicle arrived or passengers started to wait, in the order of
    /// the vehicles' arrival. Passengers alight and board sequentially:
    fn process_halting(&mut self, now: SimTime, next: SimTime) -> Vec<LegAgent> {
        let mut result = Vec::new();
        self.start_waiting(now);

        // Doors are used until the next tick starts.
        let until = next;
        // Taken and put back, so that the list keeps its capacity. Only the stops where a vehicle
        // has not finished yet stay in it, for the next tick.
        let mut stops_to_be_processed = std::mem::take(&mut self.stops_to_be_processed);
        stops_to_be_processed.sort_unstable();
        stops_to_be_processed.dedup();
        stops_to_be_processed.retain(|stop| {
            let state = self
                .stops
                .get_mut(stop)
                .expect("Only stops that a route serves are processed.");
            let mut stop_to_be_processed = false;
            for halting_vehicle in &mut state.halting_vehicles {
                halting_vehicle.doors_used_until = halting_vehicle.doors_used_until.max(now);

                if halting_vehicle.doors == Doors::Alighting
                    && Self::alight(&mut self.comp_env, halting_vehicle, until, &mut result)
                {
                    halting_vehicle.doors = Doors::Boarding { complete: false };
                }
                if let Doors::Boarding { .. } = halting_vehicle.doors {
                    let complete = Self::board(
                        &mut self.comp_env,
                        halting_vehicle,
                        &mut state.waiting_passengers,
                        until,
                    );
                    halting_vehicle.doors = Doors::Boarding { complete };
                }

                // Stop still needs to be processed if the vehicle is still boarding or alighting passengers.
                stop_to_be_processed |= halting_vehicle.doors != Doors::Boarding { complete: true };
            }
            stop_to_be_processed
        });
        self.stops_to_be_processed = stops_to_be_processed;
        result
    }

    /// The passengers who started their leg in this tick join the waiting passengers at their
    /// stops. They wait behind the others, in the order of their ids, whatever the order in which
    /// they were received.
    fn start_waiting(&mut self, now: SimTime) {
        let mut passengers = std::mem::take(&mut self.new_passengers);
        passengers.sort_unstable_by_key(|passenger| passenger.id().internal());

        for passenger in passengers {
            let access = passenger.access_stop();
            self.comp_env.events_manager_borrow_mut().process_event(
                &AgentWaitingForPtEventBuilder::default()
                    .time(now)
                    .agent(passenger.id().clone())
                    .at_stop(access.clone())
                    .destination_stop(passenger.egress_stop())
                    .build()
                    .unwrap(),
            );

            let state = self.stops.get_mut(&access).unwrap_or_else(|| {
                panic!(
                    "Agent {} waits at stop {access}, which no transit route serves.",
                    passenger.id()
                )
            });
            state.waiting_passengers.push(passenger);
            self.stops_to_be_processed.push(access);
        }
    }

    /// A vehicle leaves its stop when nobody wants to alight or board anymore and its departure
    /// time has come. Otherwise, it tries again in the next tick, or once its doors are free.
    fn depart(&mut self, now: SimTime, next: SimTime, result: &mut TransitStepResult) {
        for due in self.departing.pop(now) {
            let halting_vehicle = &mut self
                .stops
                .get_mut(&due.stop)
                .expect("A vehicle that is due to depart halts at its stop.")
                .halting_vehicles;
            let position = halting_vehicle
                .iter()
                .position(|halting_vehicle| halting_vehicle.vehicle.id() == &due.vehicle)
                .expect("A vehicle that is due to depart halts at its stop.");
            let candidate = &halting_vehicle[position];
            let ready = candidate.doors == Doors::Boarding { complete: true }
                && candidate.doors_used_until <= now;
            if !ready {
                let later_order = order(&candidate.vehicle);
                let later_departure = DepartureTracker {
                    time: candidate.doors_used_until.max(next),
                    ..due
                };
                self.departing
                    .add_with_order(later_departure, now, later_order);
                continue;
            }
            // `remove` keeps the order in which the other vehicles arrived.
            let vehicle = halting_vehicle.remove(position).vehicle;
            self.leave_stop(vehicle, now, next, result);
        }
    }

    fn leave_stop(
        &mut self,
        mut vehicle: TransitVehicle,
        now: SimTime,
        next: SimTime,
        result: &mut TransitStepResult,
    ) {
        self.comp_env.events_manager_borrow_mut().process_event(
            &VehicleDepartsAtFacilityEventBuilder::default()
                .time(now)
                .vehicle(vehicle.id().clone())
                .facility(vehicle.driver().stop().clone())
                .build()
                .unwrap(),
        );

        if vehicle.driver().upcoming_stops().is_empty() {
            result.arrived.push(self.finish_vehicle(vehicle, now));
            return;
        }

        // As for "normal teleportation: Min time for teleportation is one tick,
        // so that the result does not depend on whether that stop is on another partition.
        let earliest_arrival = next;
        vehicle
            .driver_mut()
            .notify_stop_departed(earliest_arrival, now);
        let to = self.schedule.stop_partition(vehicle.driver().stop());
        if to == self.rank {
            let order = order(&vehicle);
            self.approaching.add_with_order(vehicle, now, order);
        } else {
            result.leaving.push((to, self.send(vehicle, to, now)));
        }
    }

    /// Packs the vehicle and the attachments of its passengers, e.g. backpacks, for partition `to`.
    fn send(&mut self, vehicle: TransitVehicle, to: u32, now: SimTime) -> TransitVehicleMessage {
        // emit partition leave events for passengers before sending the vehicle to another partition
        for passenger in vehicle.passengers() {
            emit_partition_leave_events_for_agent(&mut self.comp_env, passenger.as_ref(), to, now);
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
        TransitVehicleMessage::new(vehicle, attachments)
    }

    /// The vehicle left its last stop: the driver leaves the vehicle and ends its leg. The vehicle is dropped.
    fn finish_vehicle(&mut self, vehicle: TransitVehicle, now: SimTime) -> LegAgent {
        assert!(
            vehicle.passengers().is_empty(),
            "Passengers {:?} are still in transit vehicle {} after its last stop.",
            vehicle
                .passengers()
                .iter()
                .map(|passenger| passenger.id().external())
                .collect::<Vec<_>>(),
            vehicle.id()
        );
        self.comp_env.events_manager_borrow_mut().process_event(
            &PersonLeavesVehicleEventBuilder::default()
                .time(now)
                .person(vehicle.driver().id().clone())
                .vehicle(vehicle.id().clone())
                .build()
                .unwrap(),
        );
        vehicle.into_driver()
    }

    /// Lets the passengers of the halting vehicle alight who want to at its stop, one after the other in
    /// the order in which they boarded, until the tick ends at `until`.
    fn alight(
        comp_env: &mut ThreadLocalComputationalEnvironment,
        halting: &mut HaltingVehicle,
        until: SimTime,
        alighted: &mut Vec<LegAgent>,
    ) -> bool {
        if halting.vehicle.passengers().is_empty() {
            return true;
        }
        let vehicle_id = halting.vehicle.id().clone();
        let stop = halting.vehicle.driver().stop().clone();
        let mut doors_used_until = halting.doors_used_until;
        let mut complete = true;
        let alighting = halting.vehicle.extract_passengers(|passenger| {
            // Cannot alight if others use doors until time step is over.
            if doors_used_until >= until {
                // Set complete to false, since there are still passengers for possible alighting in the next tick
                complete = false;
                return false;
            }
            if !passenger.wants_to_alight(comp_env, &stop, doors_used_until) {
                return false;
            }
            comp_env.events_manager_borrow_mut().process_event(
                &PersonLeavesVehicleEventBuilder::default()
                    .time(doors_used_until)
                    .person(passenger.id().clone())
                    .vehicle(vehicle_id.clone())
                    .build()
                    .unwrap(),
            );
            passenger.notify_left_vehicle(doors_used_until);
            doors_used_until += SimTime::from_duration(ALIGHTING_TIME);
            true
        });
        alighted.extend(alighting.into_iter().map(|passenger| passenger as LegAgent));
        halting.doors_used_until = doors_used_until;
        complete
    }

    /// Lets the passengers in `waiting` board the halting vehicle who want to, one after the other in
    /// the order in which they wait, until the tick ends at `until`.
    fn board(
        comp_env: &mut ThreadLocalComputationalEnvironment,
        halting: &mut HaltingVehicle,
        waiting: &mut Vec<PassengerAgent>,
        until: SimTime,
    ) -> bool {
        // At most stops, nobody waits. Return before cloning ids, which are reference counted.
        if waiting.is_empty() {
            return true;
        }
        let vehicle = &mut halting.vehicle;
        let vehicle_id = vehicle.id().clone();
        let driver = vehicle.driver();

        if driver.upcoming_stops().is_empty() {
            // No one is allowed to board at last stop.
            return true;
        }

        let mut doors_used_until = halting.doors_used_until;
        let mut complete = true;
        let boarding: Vec<_> = waiting
            .extract_if(.., |passenger| {
                // Cannot board if others use doors until time step is over. (See alight)
                if doors_used_until >= until {
                    // Set complete to false, since there are still passengers for possible boarding in the next tick. (See alight)
                    complete = false;
                    return false;
                }
                if !passenger.wants_to_board(
                    comp_env,
                    driver.line(),
                    driver.upcoming_stops(),
                    doors_used_until,
                ) {
                    return false;
                }
                comp_env.events_manager_borrow_mut().process_event(
                    &PersonEntersVehicleEventBuilder::default()
                        .time(doors_used_until)
                        .person(passenger.id().clone())
                        .vehicle(vehicle_id.clone())
                        .build()
                        .unwrap(),
                );
                doors_used_until += SimTime::from_duration(BOARDING_TIME);
                true
            })
            .collect();
        for passenger in boarding {
            vehicle.add_passenger(passenger);
        }
        halting.doors_used_until = doors_used_until;
        complete
    }
}

#[cfg(test)]
mod tests {
    use super::TransitEngine;
    use crate::simulation::agents::{LegAgent, PassengerAgent};
    use crate::simulation::config::TransitSimulation;
    use crate::simulation::controller::ThreadLocalComputationalEnvironment;
    use crate::simulation::events::{
        PersonEntersVehicleEvent, PersonLeavesVehicleEvent, VehicleArrivesAtFacilityEvent,
    };
    use crate::simulation::id::Id;
    use crate::simulation::pt::driver::PlannedDriver;
    use crate::simulation::pt::schedule::{
        TransitSimSchedule, give_each_departure_its_own_vehicle,
    };
    use crate::simulation::scenario::network::Network;
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

    /// The tutorial's route 1to3 departs at 06:00, 06:15 and 06:30 from stop 1, halts at stop 2a
    /// from 3:20 to 4:00 minutes later and reaches stop 3 after 9 minutes.
    const SIX: u64 = 6 * 3600;
    /// The vehicle of the 06:15 departure of route 1to3, a copy of the shared vehicle `tr_2`.
    const DEPARTURE_0615: &str = "tr_2_Blue Line_1to3_02";

    type Exchange = (u64, &'static str, String, String);
    type Exchanges = Rc<RefCell<Vec<Exchange>>>;

    fn exchange(time: u64, kind: &'static str, person: &str, vehicle: &str) -> Exchange {
        (time, kind, person.to_owned(), vehicle.to_owned())
    }

    /// One engine per partition for the tutorial schedule. With two partitions, stops 2b and 3 are
    /// on partition 1. The engines record when passengers enter and leave vehicles.
    fn tutorial_engines(parts: u32, exchanges: &Exchanges) -> Vec<TransitEngine> {
        let schedule = tutorial_schedule(parts);
        (0..parts)
            .map(|rank| {
                let mut comp_env = ThreadLocalComputationalEnvironment::default();
                let mut events = comp_env.events_manager_borrow_mut();
                for kind in ["enters", "leaves"] {
                    let exchanges = exchanges.clone();
                    let record = move |time: u64, person: &str, vehicle: &str| {
                        if !person.starts_with("pt_") {
                            exchanges
                                .borrow_mut()
                                .push(exchange(time, kind, person, vehicle));
                        }
                    };
                    if kind == "enters" {
                        events.on::<PersonEntersVehicleEvent, _>(move |e| {
                            record(e.time.as_secs(), e.person.external(), e.vehicle.external())
                        });
                    } else {
                        events.on::<PersonLeavesVehicleEvent, _>(move |e| {
                            record(e.time.as_secs(), e.person.external(), e.vehicle.external())
                        });
                    }
                }
                drop(events);
                TransitEngine::new(schedule.clone(), rank, comp_env, SimClock::new(1))
            })
            .collect()
    }

    /// Starts the departures of `tick` whose first stop is on the partition of `engine`, as the
    /// leg engine does when the activities of their drivers end.
    fn start_drivers(engine: &mut TransitEngine, tick: u64) {
        let now = SimTime::from_secs(tick);
        let schedule = engine.schedule.clone();
        for index in schedule.departures_starting_on(engine.rank) {
            if schedule.schedule().departure(index).departure_time == now {
                let driver = Box::new(PlannedDriver::new(&schedule, index, now));
                engine.receive_driver(Tick::new(tick), driver);
            }
        }
    }

    /// The tutorial schedule. With two partitions, stops 2b and 3 are on partition 1.
    fn tutorial_schedule(parts: u32) -> Arc<TransitSimSchedule> {
        tutorial_schedule_with_transit(parts, || {
            TransitSchedule::from_file(Path::new("./assets/pt_tutorial/transitschedule.xml"))
        })
    }

    fn tutorial_schedule_with_transit(
        parts: u32,
        load_transit: impl FnOnce() -> TransitSchedule,
    ) -> Arc<TransitSimSchedule> {
        let mut network = Network::from_file("./assets/pt_tutorial/multimodalnetwork.xml");
        if parts == 2 {
            for link in ["32", "33"] {
                network.get_link_mut(&Id::get_from_ext(link)).partition = 1;
            }
        }
        let mut schedule = load_transit();
        let mut garage =
            Garage::from_file(&PathBuf::from("./assets/pt_tutorial/transitVehicles.xml"));
        give_each_departure_its_own_vehicle(&mut schedule, &mut garage);
        Arc::new(TransitSimSchedule::build(
            Arc::new(schedule),
            &network,
            &garage,
            &Population::default(),
            TransitSimulation::ScheduleBasedTeleportation,
        ))
    }

    /// Runs the engines, one per partition, from 06:00 up to and including `until`. Each
    /// passenger starts its leg in its tick on the partition of its stop. Vehicles that leave for
    /// another partition arrive there after the step of the same tick, as with `send_recv`.
    /// Returns the passengers who alighted, with their tick.
    fn run(
        engines: &mut [TransitEngine],
        mut passengers: Vec<(u64, PassengerAgent)>,
        until: u64,
    ) -> Vec<(u64, LegAgent)> {
        let mut alighted = Vec::new();
        for tick in SIX..=until {
            let now = Tick::new(tick);
            for engine in engines.iter_mut() {
                start_drivers(engine, tick);
            }
            for (_, passenger) in passengers.extract_if(.., |(start, _)| *start == tick) {
                let engine = engines
                    .iter_mut()
                    .find(|engine| {
                        engine.schedule.stop_partition(&passenger.access_stop()) == engine.rank
                    })
                    .unwrap();
                engine.receive_agent(passenger);
            }
            let mut leaving = Vec::new();
            for (from, engine) in engines.iter_mut().enumerate() {
                let step = engine.do_step(now);
                let passengers = step
                    .arrived
                    .into_iter()
                    .filter(|agent| !agent.id().external().starts_with("pt_"));
                alighted.extend(passengers.map(|passenger| (tick, passenger)));
                leaving.extend(step.leaving.into_iter().map(|left| (from, left)));
            }
            for (from, (to, message)) in leaving {
                let (vehicle, _) = message.into_parts();
                engines[to as usize].receive_remote_vehicle(now, vehicle, from as u32);
            }
        }
        alighted
    }

    #[deterministic_id_test]
    fn a_passenger_rides_the_next_vehicle_of_its_line_to_its_egress_stop() {
        // With two partitions, the vehicle changes partitions on the way to stop 3.
        for parts in [1, 2] {
            let exchanges = Exchanges::default();
            let mut engines = tutorial_engines(parts, &exchanges);
            let passenger = create_pt_agent(1, "Blue Line", "1", "3", "11", "33");

            let alighted = run(&mut engines, vec![(SIX + 300, passenger)], SIX + 1800);

            assert_eq!(
                *exchanges.borrow(),
                [
                    exchange(SIX + 900, "enters", "1", DEPARTURE_0615),
                    exchange(SIX + 1440, "leaves", "1", DEPARTURE_0615),
                ]
            );
            let [(tick, passenger)] = alighted.as_slice() else {
                panic!("Expected one alighted passenger");
            };
            assert_eq!(*tick, SIX + 1440);
            assert_eq!(passenger.curr_link_id().unwrap().external(), "33");
        }
    }

    #[deterministic_id_test]
    fn a_vehicle_takes_the_passengers_who_wait_up_to_its_departure_tick() {
        let exchanges = Exchanges::default();
        let mut engines = tutorial_engines(1, &exchanges);
        let passengers = vec![
            (
                SIX + 899,
                create_pt_agent(1, "Blue Line", "1", "3", "11", "33"),
            ),
            // Starts to wait in the tick of the 06:15 departure and still takes it.
            (
                SIX + 900,
                create_pt_agent(2, "Blue Line", "1", "3", "11", "33"),
            ),
            // Comes one tick late, so it takes the 06:30 departure.
            (
                SIX + 901,
                create_pt_agent(3, "Blue Line", "1", "3", "11", "33"),
            ),
            // Comes while the 06:15 departure halts at stop 2a and boards right away.
            (
                SIX + 1110,
                create_pt_agent(4, "Blue Line", "2a", "3", "12", "33"),
            ),
        ];

        run(&mut engines, passengers, SIX + 1800);

        let boardings: Vec<_> = exchanges
            .borrow()
            .iter()
            .filter(|(_, kind, ..)| *kind == "enters")
            .cloned()
            .collect();
        assert_eq!(
            boardings,
            [
                exchange(SIX + 900, "enters", "1", DEPARTURE_0615),
                exchange(SIX + 900, "enters", "2", DEPARTURE_0615),
                exchange(SIX + 1110, "enters", "4", DEPARTURE_0615),
                exchange(SIX + 1800, "enters", "3", "tr_1_Blue Line_1to3_03"),
            ]
        );
    }

    #[deterministic_id_test]
    fn drain_returns_waiting_and_riding_passengers_but_no_drivers() {
        let mut engines = tutorial_engines(1, &Exchanges::default());
        let passengers = vec![
            (SIX, create_pt_agent(1, "Red Line", "1", "3", "11", "33")),
            (
                SIX + 899,
                create_pt_agent(2, "Blue Line", "1", "3", "11", "33"),
            ),
        ];
        run(&mut engines, passengers, SIX + 1000);

        let mut ids: Vec<_> = engines[0]
            .drain()
            .iter()
            .map(|agent| agent.id().external().to_owned())
            .collect();
        ids.sort();
        assert_eq!(ids, ["1", "2"]);
    }

    #[deterministic_id_test]
    #[should_panic(expected = "was received in tick 22140, but it is due at stop 3 in tick 22140")]
    fn a_vehicle_received_in_the_tick_of_its_arrival_panics() {
        let mut engines = tutorial_engines(2, &Exchanges::default());
        // The 06:00 departure leaves stop 2a at 06:04 for stop 3 on partition 1.
        let mut leaving = Vec::new();
        for tick in SIX..=SIX + 240 {
            start_drivers(&mut engines[0], tick);
            leaving.extend(engines[0].do_step(Tick::new(tick)).leaving);
        }
        assert_eq!(leaving.len(), 1);
        let (to, message) = leaving.pop().unwrap();
        assert_eq!(to, 1);
        let (vehicle, _) = message.into_parts();

        engines[1].receive_remote_vehicle(Tick::new(SIX + 540), vehicle, 0);
    }

    #[deterministic_id_test]
    fn a_late_vehicle_preserves_travel_time_to_the_next_stop() {
        assert_late_vehicle_arrival(tutorial_schedule(1), 200);
    }

    /// With six ticks per second, a tick does not last a whole number of nanoseconds. A vehicle
    /// still reaches the next stop in the following tick when only the minimum time between stops
    /// holds it back, even after a late departure.
    #[deterministic_id_test]
    fn a_zero_travel_time_vehicle_reaches_the_next_stop_in_the_following_tick() {
        let xml = std::fs::read_to_string("./assets/pt_tutorial/transitschedule.xml")
            .unwrap()
            .replace("arrivalOffset=\"00:03:20\"", "arrivalOffset=\"00:00:00\"");
        let transit: crate::simulation::io::xml::transit::IOTransitSchedule =
            quick_xml::de::from_str(&xml).unwrap();
        assert_late_vehicle_arrival(tutorial_schedule_with_transit(1, || transit.into()), 0);
    }

    fn assert_late_vehicle_arrival(schedule: Arc<TransitSimSchedule>, travel_secs: u64) {
        let clock = SimClock::new(6);
        let arrivals = Rc::new(RefCell::new(Vec::new()));
        let mut comp_env = ThreadLocalComputationalEnvironment::default();
        let recorded = arrivals.clone();
        comp_env
            .events_manager_borrow_mut()
            .on::<VehicleArrivesAtFacilityEvent, _>(move |e| {
                recorded
                    .borrow_mut()
                    .push((e.time, e.facility.external().to_owned()))
            });
        let mut engine = TransitEngine::new(schedule.clone(), 0, comp_env, clock);

        // The 06:00 departure starts at 06:10 instead. It leaves stop 1 right away, so its
        // scheduled arrival at stop 2a has passed. Its travel time still applies.
        let (index, _) = schedule
            .schedule()
            .indexed_departures()
            .find(|(index, _)| schedule.driver(*index).external() == "pt_Blue Line_1to3_01")
            .unwrap();
        let start = clock.secs_to_tick(SIX + 600);
        let driver = PlannedDriver::new(&schedule, index, clock.tick_to_time(start));
        engine.receive_driver(start, Box::new(driver));
        engine.do_step(start);
        let arrival = clock
            .secs_to_tick(SIX + 600 + travel_secs)
            .max(start.next());
        for tick in start.next().value()..=arrival.value() {
            engine.do_step(Tick::new(tick));
        }

        assert_eq!(
            *arrivals.borrow(),
            [
                (clock.tick_to_time(start), "1".to_owned()),
                (clock.tick_to_time(arrival), "2a".to_owned()),
            ]
        );
    }
}
