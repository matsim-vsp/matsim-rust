//! Runs configured transit services against their scheduled stop times.

use crate::simulation::Identifiable;
use crate::simulation::agents::agent::SimulationAgent;
use crate::simulation::agents::{
    AgentEvent, EndTime, EnvironmentalEventObserver, SimulationAgentLogic,
};
use crate::simulation::controller::ThreadLocalComputationalEnvironment;
use crate::simulation::events::{
    LinkEnterEventBuilder, LinkLeaveEventBuilder, PersonArrivalEventBuilder,
    PersonDepartureEventBuilder, PersonEntersVehicleEventBuilder, PersonLeavesVehicleEventBuilder,
    VehicleEntersTrafficEventBuilder, VehicleLeavesTrafficEventBuilder,
};
use crate::simulation::id::Id;
use crate::simulation::pt::driver::{StopOutcome, TransitDriver, serve_stop};
use crate::simulation::pt::runs::RunLeg;
use crate::simulation::pt::stops::TransitStops;
use crate::simulation::scenario::ScenarioCore;
use crate::simulation::scenario::network::{Link, Network};
use crate::simulation::scenario::population::InternalPerson;
use crate::simulation::scenario::transit::{TransitLine, TransitRoute, TransitSchedule};
use crate::simulation::scenario::vehicles::{Garage, InternalVehicle};
use crate::simulation::time::{SimClock, SimTime, Tick};
use crate::simulation::time_queue::TimeQueue;
use crate::simulation::vehicles::SimulationVehicle;
use nohash_hasher::IntMap;
use nohash_hasher::IntSet;
use std::sync::Arc;
use std::time::Duration;

struct StopEvent {
    driver: Id<InternalPerson>,
    time: SimTime,
}

struct LinkEvent {
    time: SimTime,
    from: Id<Link>,
    to: Id<Link>,
    vehicle: Id<InternalVehicle>,
}

impl EndTime for LinkEvent {
    fn end_time(&self, _now: SimTime) -> SimTime {
        self.time
    }
}

impl EndTime for StopEvent {
    fn end_time(&self, _now: SimTime) -> SimTime {
        self.time
    }
}

pub(crate) struct TimetableTransitEngine {
    waiting_drivers: TimeQueue<SimulationAgent, InternalPerson>,
    stop_events: TimeQueue<StopEvent, InternalPerson>,
    link_events: TimeQueue<LinkEvent, InternalVehicle>,
    active: IntMap<Id<InternalPerson>, SimulationVehicle>,
    route_link_positions: IntMap<Id<InternalPerson>, usize>,
    network: Arc<Network>,
    schedule: Arc<TransitSchedule>,
    deterministic_modes: IntSet<Id<String>>,
    garage: Arc<Garage>,
    comp_env: ThreadLocalComputationalEnvironment,
    clock: SimClock,
    create_link_events: bool,
}

impl TimetableTransitEngine {
    pub(crate) fn new(
        scenario: &ScenarioCore,
        rank: u32,
        comp_env: ThreadLocalComputationalEnvironment,
        clock: SimClock,
        start: SimTime,
        iteration: u32,
    ) -> Self {
        let mut engine = Self {
            waiting_drivers: TimeQueue::new(),
            stop_events: TimeQueue::new(),
            link_events: TimeQueue::new(),
            active: IntMap::default(),
            route_link_positions: IntMap::default(),
            network: scenario.network.clone(),
            schedule: scenario.transit_schedule.clone(),
            deterministic_modes: scenario
                .config
                .transit()
                .deterministic_service_modes
                .iter()
                .map(|mode| Id::get_from_ext(mode))
                .collect(),
            garage: scenario.garage.clone(),
            comp_env,
            clock,
            create_link_events: scenario.config.transit().create_link_events_interval > 0
                && iteration.is_multiple_of(scenario.config.transit().create_link_events_interval),
        };
        for run in scenario.transit_runs.runs().iter().filter(|run| {
            run.is_deterministic() && scenario.network.get_link(run.start_link()).partition == rank
        }) {
            let RunLeg::Service { departure_time, .. } = &run.legs[0] else {
                unreachable!("timetable services have no deadheads")
            };
            let driver =
                SimulationAgent::new(Box::new(TransitDriver::new(run.clone(), &engine.garage)));
            engine.waiting_drivers.add_with_order(
                driver,
                start.max(*departure_time),
                run.driver.internal(),
            );
        }
        engine
    }

    pub(crate) fn serves(&self, agent: &SimulationAgent) -> bool {
        let Some(pt_route) = agent
            .curr_leg()
            .route
            .as_ref()
            .and_then(|route| route.as_pt())
        else {
            return false;
        };
        let Some(line_id) =
            Id::<TransitLine>::try_get_from_ext(&pt_route.description.transit_line_id)
        else {
            return false;
        };
        let Some(route_id) =
            Id::<TransitRoute>::try_get_from_ext(&pt_route.description.transit_route_id)
        else {
            return false;
        };
        self.schedule
            .lines()
            .get(&line_id)
            .and_then(|line| line.routes.get(&route_id))
            .is_some_and(|route| self.deterministic_modes.contains(&route.transport_mode))
    }

    pub(crate) fn drain(&mut self) -> Vec<SimulationAgent> {
        self.waiting_drivers
            .drain()
            .into_iter()
            .chain(
                self.active
                    .drain()
                    .flat_map(|(_, vehicle)| vehicle.into_agents()),
            )
            .collect()
    }

    pub(crate) fn do_step(&mut self, now: Tick, stops: &mut TransitStops) -> Vec<SimulationAgent> {
        let now = self.clock.tick_to_time(now);
        let mut completed = Vec::new();
        for event in self.link_events.pop(now) {
            let mut events = self.comp_env.events_manager_borrow_mut();
            events.process_event(
                &LinkLeaveEventBuilder::default()
                    .time(now)
                    .link(event.from)
                    .vehicle(event.vehicle.clone())
                    .build()
                    .unwrap(),
            );
            events.process_event(
                &LinkEnterEventBuilder::default()
                    .time(now)
                    .link(event.to)
                    .vehicle(event.vehicle)
                    .build()
                    .unwrap(),
            );
        }
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
                if self.create_link_events {
                    let route = driver.transit_driver().unwrap().run().legs.last().unwrap();
                    let RunLeg::Service { route, .. } = route else {
                        unreachable!()
                    };
                    events.process_event(
                        &VehicleEntersTrafficEventBuilder::default()
                            .time(now)
                            .person(driver.id().clone())
                            .vehicle(vehicle_id.clone())
                            .link(driver.curr_link_id().unwrap().clone())
                            .network_mode(route.transport_mode.clone())
                            .build()
                            .unwrap(),
                    );
                }
            }
            let key = driver.id().clone();
            let route_position = driver
                .transit_driver()
                .unwrap()
                .run()
                .legs
                .last()
                .and_then(|leg| match leg {
                    RunLeg::Service { route, .. } => route
                        .links
                        .iter()
                        .position(|link| link == driver.transit_driver().unwrap().next_stop_link()),
                    RunLeg::Deadhead { .. } => None,
                })
                .unwrap_or(0);
            let vehicle = self.garage.unpark_veh(driver, vehicle_id);
            self.route_link_positions
                .insert(key.clone(), route_position);
            self.schedule_stop(&vehicle, key.clone(), now, true);
            self.active.insert(key, vehicle);
        }

        for event in self.stop_events.pop(now) {
            let event_time = event.time;
            let Some(mut vehicle) = self.active.remove(&event.driver) else {
                continue;
            };
            let stop_link = vehicle
                .driver()
                .transit_driver()
                .unwrap()
                .next_stop_link()
                .clone();
            while vehicle.curr_link_id() != Some(&stop_link) {
                assert!(
                    vehicle.peek_next_route_element().is_some(),
                    "Transit route ends before its next scheduled stop on link {stop_link}."
                );
                *self.route_link_positions.get_mut(&event.driver).unwrap() += 1;
                vehicle.notify_event(&mut AgentEvent::LeftLink(), event_time);
            }
            let outcome = {
                let link = vehicle.curr_link_id().unwrap().clone();
                let mut events = self.comp_env.events_manager_borrow_mut();
                serve_stop(&mut vehicle, &link, event_time, stops, &mut events, true)
            };
            match outcome {
                StopOutcome::Dwell { seconds, .. } => {
                    let next = event_time.saturating_add(Duration::from_secs_f64(seconds));
                    self.reschedule(event.driver.clone(), next);
                    self.active.insert(event.driver, vehicle);
                }
                StopOutcome::Departed => {
                    let driver = vehicle.driver().transit_driver().unwrap();
                    if driver.is_finished() {
                        self.route_link_positions.remove(&event.driver);
                        self.finish_vehicle(event_time, vehicle, &mut completed);
                    } else {
                        let service_ended = match driver.run().legs.last().unwrap() {
                            RunLeg::Service { route, .. } => {
                                driver.next_stop_index() >= route.stops.len()
                            }
                            RunLeg::Deadhead { .. } => false,
                        };
                        if self.create_link_events && service_ended {
                            let RunLeg::Service { route, .. } = driver.run().legs.last().unwrap()
                            else {
                                unreachable!()
                            };
                            self.comp_env.events_manager_borrow_mut().process_event(
                                &VehicleLeavesTrafficEventBuilder::default()
                                    .time(event_time)
                                    .person(vehicle.driver().id().clone())
                                    .vehicle(vehicle.id().clone())
                                    .link(vehicle.curr_link_id().unwrap().clone())
                                    .network_mode(route.transport_mode.clone())
                                    .build()
                                    .unwrap(),
                            );
                        }
                        if !service_ended && self.create_link_events {
                            self.schedule_synthetic_links(&vehicle, &event.driver, event_time);
                        }
                        self.schedule_stop(&vehicle, event.driver.clone(), event_time, false);
                        self.active.insert(event.driver, vehicle);
                    }
                }
                StopOutcome::NoStop => panic!("A timetable event must always target a route stop."),
            }
        }
        completed
    }

    fn schedule_stop(
        &mut self,
        vehicle: &SimulationVehicle,
        driver: Id<InternalPerson>,
        now: SimTime,
        first: bool,
    ) {
        let transit_driver = vehicle.driver().transit_driver().unwrap();
        let RunLeg::Service {
            route,
            departure_time,
            ..
        } = transit_driver.run().legs.last().unwrap()
        else {
            unreachable!("timetable services have no deadheads")
        };
        let Some(stop) = route.stops.get(transit_driver.next_stop_index()) else {
            return;
        };
        let time = if first {
            now
        } else {
            let offset = stop
                .arrival_offset
                .or(stop.departure_offset)
                .unwrap_or_default();
            now.max(departure_time.saturating_add(offset))
        };
        self.reschedule(driver, time);
    }

    fn reschedule(&mut self, driver: Id<InternalPerson>, time: SimTime) {
        self.stop_events.add_with_order(
            StopEvent {
                driver: driver.clone(),
                time,
            },
            time,
            driver.internal(),
        );
    }

    fn finish_vehicle(
        &mut self,
        now: SimTime,
        mut vehicle: SimulationVehicle,
        completed: &mut Vec<SimulationAgent>,
    ) {
        let mut events = self.comp_env.events_manager_borrow_mut();
        if self.create_link_events {
            let driver = vehicle.driver().transit_driver().unwrap();
            let RunLeg::Service { route, .. } = driver.run().legs.last().unwrap() else {
                unreachable!()
            };
            events.process_event(
                &VehicleLeavesTrafficEventBuilder::default()
                    .time(now)
                    .person(vehicle.driver().id().clone())
                    .vehicle(vehicle.id().clone())
                    .link(vehicle.curr_link_id().unwrap().clone())
                    .network_mode(route.transport_mode.clone())
                    .build()
                    .unwrap(),
            );
        }
        events.process_event(
            &PersonLeavesVehicleEventBuilder::default()
                .time(now)
                .person(vehicle.driver().id().clone())
                .vehicle(vehicle.id().clone())
                .build()
                .unwrap(),
        );
        events.process_event(
            &PersonArrivalEventBuilder::default()
                .time(now)
                .person(vehicle.driver().id().clone())
                .link(vehicle.curr_link_id().unwrap().clone())
                .leg_mode(vehicle.driver().curr_leg().mode.clone())
                .build()
                .unwrap(),
        );
        vehicle.driver_mut().advance_plan(now);
        completed.extend(vehicle.into_agents());
    }

    fn schedule_synthetic_links(
        &mut self,
        vehicle: &SimulationVehicle,
        driver_id: &Id<InternalPerson>,
        departure_time: SimTime,
    ) {
        let driver = vehicle.driver().transit_driver().unwrap();
        let RunLeg::Service {
            route,
            departure_time: scheduled,
            ..
        } = driver.run().legs.last().unwrap()
        else {
            unreachable!()
        };
        let position = self.route_link_positions[driver_id];
        let Some(next_stop) = route.stops.get(driver.next_stop_index()) else {
            return;
        };
        if route.links[position] == next_stop.link {
            return;
        }
        let Some(end) = route
            .links
            .iter()
            .enumerate()
            .skip(position + 1)
            .find_map(|(index, link)| (link == &next_stop.link).then_some(index))
        else {
            return;
        };
        let links = &route.links[position..=end];
        if links.len() < 2 {
            return;
        }
        let arrival_offset = next_stop
            .arrival_offset
            .or(next_stop.departure_offset)
            .unwrap_or_default();
        let arrival_time = departure_time.max(scheduled.saturating_add(arrival_offset));
        let travel_time = arrival_time.duration_since(departure_time).as_secs_f64();
        let total_length: f64 = links
            .iter()
            .skip(1)
            .map(|link| self.network.get_link(link).length)
            .sum();
        let seconds_per_meter = if total_length > 0.0 {
            travel_time / total_length
        } else {
            0.0
        };
        let mut travelled = 0.0;
        for pair in links.windows(2) {
            let at = departure_time
                .saturating_add(Duration::from_secs_f64(travelled * seconds_per_meter));
            self.link_events.add(
                LinkEvent {
                    time: at,
                    from: pair[0].clone(),
                    to: pair[1].clone(),
                    vehicle: vehicle.id().clone(),
                },
                at,
            );
            travelled += self.network.get_link(&pair[1]).length;
        }
    }
}
