use crate::simulation::agents::{EndTime, LegAgent, PassengerAgent};
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
use crate::simulation::messaging::messages::TransitVehicleMessage;
use crate::simulation::messaging::partition_change::{
    PartitionChangeContext, PartitionChangeEntity,
};
use crate::simulation::pt::driver::PlannedDriver;
use crate::simulation::pt::schedule::{DepartureIndex, TransitSimSchedule};
use crate::simulation::time::{SimClock, SimTime, Tick};
use crate::simulation::time_queue::TimeQueue;
use crate::simulation::vehicles::TransitVehicle;
use std::sync::Arc;

/// Moves the vehicles of schedule-based transit from stop to stop without the network, like SBB's
/// deterministic transit engine in MATSim. Drivers decide when their vehicles arrive and depart,
/// passengers decide whether they board and alight. The engine carries that out and emits the
/// events.
pub(crate) struct TransitEngine {
    schedule: Arc<TransitSimSchedule>,
    rank: u32,
    comp_env: ThreadLocalComputationalEnvironment,
    clock: SimClock,
    /// Departures whose first stop is on this partition, sorted by time and departure index.
    /// TODO: This should not be done by the transit engine. PT agents should start with an activity. They start their leg when their first leg is scheduled.
    starts: Vec<DepartureIndex>,
    next_start: usize,
    /// Vehicles on their way to a stop, due at their arrival.
    approaching: TimeQueue<TransitVehicle, ()>,
    /// Vehicles that halt at a stop, due at their departure.
    departing: TimeQueue<HaltingVehicle, ()>,
    /// The state of each stop, indexed by the internal id of its facility.
    stops: Vec<StopState>,
    /// Passengers who started their leg in this tick.
    new_passengers: Vec<PassengerAgent>,
}

/// What leaves the transit engine in a step.
#[derive(Default)]
pub(crate) struct TransitStepResult {
    /// Passengers who alighted. They end their leg.
    pub(crate) alighted: Vec<PassengerAgent>,
    /// Vehicles that left for a stop on another partition, with that partition.
    pub(crate) leaving: Vec<(u32, TransitVehicleMessage)>,
}

/// A vehicle that halts at a stop, due at its departure.
struct HaltingVehicle {
    vehicle: TransitVehicle,
    /// The stop, as an index into [`TransitEngine::stops`].
    stop: usize,
    arrival: SimTime,
}

/// A passenger who waits at a stop since `since`.
struct WaitingPassenger {
    since: SimTime,
    passenger: PassengerAgent,
}

#[derive(Default)]
struct StopState {
    /// Ordered by the start of waiting and the id of the passenger (`R4`).
    waiting: Vec<WaitingPassenger>,
}

impl EndTime for TransitVehicle {
    fn end_time(&self, _now: SimTime) -> SimTime {
        self.driver().next_time()
    }
}

impl EndTime for HaltingVehicle {
    fn end_time(&self, _now: SimTime) -> SimTime {
        self.vehicle.driver().next_time()
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
        let mut starts: Vec<_> = schedule
            .indexed_departures()
            .filter(|(_, departure)| schedule.route(departure.route).stops[0].partition == rank)
            .map(|(index, _)| index)
            .collect();
        starts.sort_by_key(|&index| (schedule.departure(index).time, index));

        // Create empty state for each stop.
        let stops = std::iter::repeat_with(StopState::default)
            .take(schedule.num_stops())
            .collect();

        Self {
            schedule,
            rank,
            comp_env,
            clock,
            starts,
            next_start: 0,
            approaching: TimeQueue::new(),
            departing: TimeQueue::new(),
            stops,
            new_passengers: Vec::new(),
        }
    }

    /// An agent starts a transit leg. It waits at its stop from the start of this tick's step on,
    /// so a vehicle that arrives or departs in this tick can take it.
    pub(crate) fn receive_agent(&mut self, agent: PassengerAgent) {
        self.new_passengers.push(agent);
    }

    /// A vehicle arrives from partition `from`. It is received after this tick's step, so its
    /// arrival at the next stop must be due in a later tick (`R3`).
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

    /// Performs one step and returns the passengers who alighted and the vehicles that left for
    /// another partition.
    #[hotpath::measure]
    pub(crate) fn do_step(&mut self, now: Tick) -> TransitStepResult {
        let now = self.clock.tick_to_time(now);
        let mut result = TransitStepResult::default();

        self.start_departures(now);
        self.start_waiting(now);
        for vehicle in self.approaching.pop(now) {
            self.arrive(vehicle, now, &mut result.alighted);
        }
        for halting in self.departing.pop(now) {
            self.depart(halting, now, &mut result.leaving);
        }
        result
    }

    /// Returns all agents that still wait or ride, e.g. at the end of the mobsim. Drivers are
    /// dropped, because they have no person (`O4`).
    // TODO: SBB reports passengers in a vehicle as stuck on the link of the next stop. Here, they
    // report the link of their access stop, because they do not know where the vehicle is.
    pub(crate) fn drain(&mut self) -> Vec<LegAgent> {
        let mut agents = std::mem::take(&mut self.new_passengers);
        for state in &mut self.stops {
            agents.extend(state.waiting.drain(..).map(|waiting| waiting.passenger));
        }
        for mut halting in self.departing.drain() {
            agents.extend(halting.vehicle.take_passengers());
        }
        for mut vehicle in self.approaching.drain() {
            agents.extend(vehicle.take_passengers());
        }
        agents.into_iter().map(|agent| agent as LegAgent).collect()
    }

    fn start_departures(&mut self, now: SimTime) {
        let schedule = self.schedule.clone();
        while let Some(&index) = self.starts.get(self.next_start) {
            let departure = schedule.departure(index);
            if departure.time > now {
                break;
            }
            self.next_start += 1;

            let route = schedule.route(departure.route);
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

            let driver = Box::new(PlannedDriver::new(&schedule, index, now));
            let vehicle = TransitVehicle::new(departure.vehicle.clone(), driver);
            let order = order(&vehicle);
            self.approaching.add_with_order(vehicle, now, order);
        }
    }

    fn arrive(
        &mut self,
        mut vehicle: TransitVehicle,
        now: SimTime,
        alighted: &mut Vec<PassengerAgent>,
    ) {
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

        // Passengers alight before others board.
        for mut passenger in vehicle.take_passengers() {
            if passenger.wants_to_alight(&mut self.comp_env, &stop, now) {
                self.comp_env.events_manager_borrow_mut().process_event(
                    &PersonLeavesVehicleEventBuilder::default()
                        .time(now)
                        .person(passenger.id().clone())
                        .vehicle(vehicle.id().clone())
                        .build()
                        .unwrap(),
                );
                passenger.notify_left_vehicle(now);
                alighted.push(passenger);
            } else {
                vehicle.add_passenger(passenger);
            }
        }

        let index = stop.internal() as usize;
        self.board(&mut vehicle, index, None, now);

        let order = order(&vehicle);
        let halting = HaltingVehicle {
            vehicle,
            stop: index,
            arrival: now,
        };
        self.departing.add_with_order(halting, now, order);
    }

    fn depart(
        &mut self,
        halting: HaltingVehicle,
        now: SimTime,
        leaving: &mut Vec<(u32, TransitVehicleMessage)>,
    ) {
        let HaltingVehicle {
            mut vehicle,
            stop,
            arrival,
        } = halting;
        // The passengers who were at the stop when the vehicle arrived have been asked then.
        self.board(&mut vehicle, stop, Some(arrival), now);

        self.comp_env.events_manager_borrow_mut().process_event(
            &VehicleDepartsAtFacilityEventBuilder::default()
                .time(now)
                .vehicle(vehicle.id().clone())
                .facility(vehicle.driver().stop().clone())
                .build()
                .unwrap(),
        );

        if vehicle.driver().upcoming_stops().is_empty() {
            self.finish(vehicle, now);
            return;
        }

        // A vehicle needs at least one tick to the next stop, so that the result does not depend on
        // whether that stop is on another partition.
        let earliest_arrival = now + SimTime::from_duration(self.clock.tick_length());
        vehicle
            .driver_mut()
            .notify_stop_departed(earliest_arrival, now);
        let to = self.schedule.stop_partition(vehicle.driver().stop());
        if to == self.rank {
            let order = order(&vehicle);
            self.approaching.add_with_order(vehicle, now, order);
        } else {
            leaving.push((to, self.send(vehicle, to, now)));
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

    /// The vehicle left its last stop: the driver leaves the vehicle and ends its leg.
    fn finish(&mut self, vehicle: TransitVehicle, now: SimTime) {
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
        let driver = vehicle.driver();
        let mut events = self.comp_env.events_manager_borrow_mut();
        events.process_event(
            &PersonLeavesVehicleEventBuilder::default()
                .time(now)
                .person(driver.id().clone())
                .vehicle(vehicle.id().clone())
                .build()
                .unwrap(),
        );
        events.process_event(
            &PersonArrivalEventBuilder::default()
                .time(now)
                .person(driver.id().clone())
                .link(driver.destination_link().clone())
                .leg_mode(driver.mode().clone())
                .build()
                .unwrap(),
        );
    }

    /// Lets the passengers at `stop` board who want to take `vehicle`, in the order in which they
    /// wait. With `after`, only those are asked who started to wait after that time. At its last
    /// stop, a vehicle takes nobody.
    fn board(
        &mut self,
        vehicle: &mut TransitVehicle,
        stop: usize,
        after: Option<SimTime>,
        now: SimTime,
    ) {
        let driver = vehicle.driver();
        if driver.upcoming_stops().is_empty() {
            return;
        }
        let waiting = &mut self.stops[stop].waiting;
        // The list is ordered by the start of waiting, so the passengers to ask are at its end.
        let first = after.map_or(0, |after| {
            waiting.partition_point(|waiting| waiting.since <= after)
        });
        let mut boarding = Vec::new();
        for candidate in waiting.split_off(first) {
            let mut passenger = candidate.passenger;
            if passenger.wants_to_board(
                &mut self.comp_env,
                driver.line(),
                driver.upcoming_stops(),
                now,
            ) {
                boarding.push(passenger);
            } else {
                waiting.push(WaitingPassenger {
                    since: candidate.since,
                    passenger,
                });
            }
        }

        for passenger in boarding {
            self.comp_env.events_manager_borrow_mut().process_event(
                &PersonEntersVehicleEventBuilder::default()
                    .time(now)
                    .person(passenger.id().clone())
                    .vehicle(vehicle.id().clone())
                    .build()
                    .unwrap(),
            );
            vehicle.add_passenger(passenger);
        }
    }

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

            let state = self
                .stops
                .get_mut(access.internal() as usize)
                .unwrap_or_else(|| {
                    panic!(
                        "Agent {} waits at stop {access}, which no transit route serves.",
                        passenger.id()
                    )
                });
            state.waiting.push(WaitingPassenger {
                since: now,
                passenger,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::TransitEngine;
    use crate::simulation::agents::PassengerAgent;
    use crate::simulation::controller::ThreadLocalComputationalEnvironment;
    use crate::simulation::events::{PersonEntersVehicleEvent, PersonLeavesVehicleEvent};
    use crate::simulation::id::Id;
    use crate::simulation::pt::schedule::TransitSimSchedule;
    use crate::simulation::scenario::network::Network;
    use crate::simulation::scenario::population::Population;
    use crate::simulation::scenario::transit::TransitSchedule;
    use crate::simulation::scenario::vehicles::Garage;
    use crate::simulation::time::{SimClock, Tick};
    use crate::test_utils::create_pt_agent;
    use macros::deterministic_id_test;
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};
    use std::rc::Rc;
    use std::sync::Arc;

    /// The tutorial's route 1to3 departs at 06:00, 06:15 and 06:30 from stop 1, halts at stop 2a
    /// from 3:20 to 4:00 minutes later and reaches stop 3 after 9 minutes.
    const SIX: u64 = 6 * 3600;

    type Exchange = (u64, &'static str, String, String);
    type Exchanges = Rc<RefCell<Vec<Exchange>>>;

    fn exchange(time: u64, kind: &'static str, person: &str, vehicle: &str) -> Exchange {
        (time, kind, person.to_owned(), vehicle.to_owned())
    }

    /// One engine per partition for the tutorial schedule. With two partitions, stops 2b and 3 are
    /// on partition 1. The engines record when passengers enter and leave vehicles.
    fn tutorial_engines(parts: u32, exchanges: &Exchanges) -> Vec<TransitEngine> {
        let mut network = Network::from_file("./assets/pt_tutorial/multimodalnetwork.xml");
        if parts == 2 {
            for link in ["32", "33"] {
                network.get_link_mut(&Id::get_from_ext(link)).partition = 1;
            }
        }
        let schedule =
            TransitSchedule::from_file(Path::new("./assets/pt_tutorial/transitschedule.xml"));
        let garage = Garage::from_file(&PathBuf::from("./assets/pt_tutorial/transitVehicles.xml"));
        let schedule = Arc::new(TransitSimSchedule::build(
            &schedule,
            &network,
            &garage,
            &Population::default(),
        ));

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

    /// Runs the engines, one per partition, from 06:00 up to and including `until`. Each
    /// passenger starts its leg in its tick on the partition of its stop. Vehicles that leave for
    /// another partition arrive there after the step of the same tick, as with `send_recv`.
    /// Returns the passengers who alighted, with their tick.
    fn run(
        engines: &mut [TransitEngine],
        mut passengers: Vec<(u64, PassengerAgent)>,
        until: u64,
    ) -> Vec<(u64, PassengerAgent)> {
        let mut alighted = Vec::new();
        for tick in SIX..=until {
            let now = Tick::new(tick);
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
                alighted.extend(step.alighted.into_iter().map(|passenger| (tick, passenger)));
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
                    exchange(SIX + 900, "enters", "1", "tr_2"),
                    exchange(SIX + 1440, "leaves", "1", "tr_2"),
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
            // Comes while the 06:15 departure halts at stop 2a and boards when it departs.
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
                exchange(SIX + 900, "enters", "1", "tr_2"),
                exchange(SIX + 900, "enters", "2", "tr_2"),
                exchange(SIX + 1140, "enters", "4", "tr_2"),
                exchange(SIX + 1800, "enters", "3", "tr_1"),
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
            leaving.extend(engines[0].do_step(Tick::new(tick)).leaving);
        }
        assert_eq!(leaving.len(), 1);
        let (to, message) = leaving.pop().unwrap();
        assert_eq!(to, 1);
        let (vehicle, _) = message.into_parts();

        engines[1].receive_remote_vehicle(Tick::new(SIX + 540), vehicle, 0);
    }
}
