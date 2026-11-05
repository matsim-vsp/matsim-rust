use crate::simulation::Identifiable;
use crate::simulation::agents::{
    ActivityAgent, ActivityLogic, LegAgent, LegLogic, NetworkDriverAgent, NextPhase,
    PassengerAgent, SimulationAgentLogic, TeleportationAgent, TransitDriverAgent,
    TransitDriverLogic,
};
use crate::simulation::controller::ThreadLocalComputationalEnvironment;
use crate::simulation::id::Id;
use crate::simulation::pt::schedule::TransitSimSchedule;
use crate::simulation::scenario::Coordinate;
use crate::simulation::scenario::network::Link;
use crate::simulation::scenario::population::InternalPerson;
use crate::simulation::scenario::transit::{
    DepartureIndex, TransitDeparture, TransitLine, TransitRoute, TransitRouteStop, TransitSchedule,
    TransitStopFacility,
};
use crate::simulation::scenario::vehicles::InternalVehicle;
use crate::simulation::time::SimTime;
use std::sync::Arc;

/// The activities of drivers emit no events, so their coordinate is never reported.
static NO_COORDINATE: Coordinate = Coordinate {
    x: 0.0,
    y: 0.0,
    z: 0.0,
};

/// Arrival and departure at a stop.
#[derive(Clone, Copy)]
struct StopTimes {
    arrival: SimTime,
    departure: SimTime,
}

/// The scheduled times of `departure` at `stop`.
fn scheduled_times(departure: &TransitDeparture, stop: &TransitRouteStop) -> StopTimes {
    let offset = |offset: Option<std::time::Duration>| {
        let offset =
            offset.expect("TransitSimSchedule::build checks that every stop has an offset.");
        departure.departure_time + SimTime::from_duration(offset)
    };
    StopTimes {
        arrival: offset(stop.arrival()),
        departure: offset(stop.departure()),
    }
}

/// The link where a driver starts its departure.
fn start_link<'a>(schedule: &'a TransitSchedule, route: &'a TransitRoute) -> &'a Id<Link> {
    schedule
        .start_link(route)
        .expect("TransitSimSchedule::build checks that every stop has a link.")
}

/// The single place where the times of a vehicle at a stop are computed from the scheduled ones.
/// Like SBB, a vehicle never arrives before `earliest_arrival` and never departs before its arrival
/// or its scheduled departure. Delays will hook in here.
fn stop_times(scheduled: StopTimes, earliest_arrival: SimTime) -> StopTimes {
    let arrival = scheduled.arrival.max(earliest_arrival);
    StopTimes {
        arrival,
        departure: scheduled.departure.max(arrival),
    }
}

/// A driver before its departure. Like MATSim's `TransitDriverAgentImpl`, it waits in an activity
/// at the start link of the route until the departure time. Then it copies its departure into a
/// [`PlannedDriver`]. Until then, it only holds the schedule and the index of its departure.
pub struct WaitingDriver {
    schedule: Arc<TransitSimSchedule>,
    departure: DepartureIndex,
}

impl WaitingDriver {
    pub fn new_agent(
        schedule: Arc<TransitSimSchedule>,
        departure: DepartureIndex,
    ) -> ActivityAgent {
        Box::new(Self {
            schedule,
            departure,
        })
    }

    fn departure(&self) -> &TransitDeparture {
        self.schedule.schedule().departure(self.departure)
    }
}

impl Identifiable<InternalPerson> for WaitingDriver {
    fn id(&self) -> &Id<InternalPerson> {
        self.schedule.driver(self.departure)
    }
}

impl SimulationAgentLogic for WaitingDriver {
    fn into_person(self: Box<Self>) -> Option<InternalPerson> {
        None
    }
}

impl ActivityLogic for WaitingDriver {
    fn act_type(&self) -> &Id<String> {
        self.schedule.driver_activity_type()
    }
    fn link_id(&self) -> &Id<Link> {
        let schedule = self.schedule.schedule();
        let route = schedule.route(schedule.departure_route(self.departure));
        start_link(schedule, route)
    }
    fn coord(&self) -> &Coordinate {
        &NO_COORDINATE
    }
    fn has_next_leg(&self) -> bool {
        true
    }
    fn has_activity_events(&self) -> bool {
        false
    }
    fn end_time(&self, _activity_start: SimTime) -> SimTime {
        self.departure().departure_time
    }
    fn next_wakeup(&self, _now: SimTime) -> Option<SimTime> {
        None
    }
    fn notify_started(
        &mut self,
        _comp_env: &mut ThreadLocalComputationalEnvironment,
        _now: SimTime,
    ) {
    }
    fn notify_wakeup(
        &mut self,
        _comp_env: &mut ThreadLocalComputationalEnvironment,
        _end_time: SimTime,
        _now: SimTime,
    ) {
    }
    fn notify_finished(&mut self, _now: SimTime) {}
    fn finish_activity(self: Box<Self>, now: SimTime) -> NextPhase {
        NextPhase::Leg(Box::new(PlannedDriver::new(
            &self.schedule,
            self.departure,
            now,
        )))
    }
}

/// A driver with a plan of its own, which it copies from the schedule when its departure starts.
/// Afterwards, it does not depend on the schedule. Like the drivers of SBB's deterministic transit
/// engine, it follows the scheduled times.
pub struct PlannedDriver {
    plan: DriverPlan,
    /// Index of the stop at which the vehicle halts or which it approaches.
    stop: usize,
    /// The arrival at the current stop while approaching it, the departure while halting there.
    next_time: SimTime,
}

/// The departure of a driver: what its leg and its events need and, for each stop, the facility,
/// the link and the scheduled times.
struct DriverPlan {
    id: Id<InternalPerson>,
    mode: Id<String>,
    vehicle: Id<InternalVehicle>,
    line: Id<TransitLine>,
    route: Id<TransitRoute>,
    departure: Id<TransitDeparture>,
    start_link: Id<Link>,
    destination_link: Id<Link>,
    /// The type of the activity after the departure.
    activity_type: Id<String>,
    stops: Vec<Id<TransitStopFacility>>,
    stop_links: Vec<Id<Link>>,
    scheduled: Vec<StopTimes>,
}

impl PlannedDriver {
    /// A driver who starts `departure` at `now` and approaches the first stop.
    pub fn new(schedule: &TransitSimSchedule, index: DepartureIndex, now: SimTime) -> Self {
        let transit = schedule.schedule();
        let departure = transit.departure(index);
        let route = transit.route(transit.departure_route(index));
        let plan = DriverPlan {
            id: schedule.driver(index).clone(),
            mode: route.transport_mode.clone(),
            vehicle: schedule.vehicle(index).clone(),
            line: route.line.clone(),
            route: route.id.clone(),
            departure: departure.id.clone(),
            start_link: start_link(transit, route).clone(),
            destination_link: transit
                .end_link(route)
                .expect("TransitSimSchedule::build checks that every stop has a link.")
                .clone(),
            activity_type: schedule.driver_activity_type().clone(),
            stops: route.stop_facilities.clone(),
            stop_links: route
                .stop_facilities
                .iter()
                .map(|facility| {
                    transit
                        .stop_link(facility)
                        .expect("TransitSimSchedule::build checks that every stop has a link.")
                        .clone()
                })
                .collect(),
            scheduled: route
                .stops
                .iter()
                .map(|stop| scheduled_times(departure, stop))
                .collect(),
        };
        let next_time = stop_times(plan.scheduled[0], now).arrival;
        Self {
            plan,
            stop: 0,
            next_time,
        }
    }
}

impl Identifiable<InternalPerson> for PlannedDriver {
    fn id(&self) -> &Id<InternalPerson> {
        &self.plan.id
    }
}

impl SimulationAgentLogic for PlannedDriver {
    fn into_person(self: Box<Self>) -> Option<InternalPerson> {
        None
    }
}

impl LegLogic for PlannedDriver {
    fn mode(&self) -> &Id<String> {
        &self.plan.mode
    }
    fn routing_mode(&self) -> Option<&Id<String>> {
        // Like in MATSim, transit drivers have no routing mode.
        None
    }
    fn departure_link(&self) -> &Id<Link> {
        &self.plan.start_link
    }
    /// The link of the current stop. From the last stop on, the link where the leg ends.
    fn curr_link_id(&self) -> Option<&Id<Link>> {
        if self.upcoming_stops().is_empty() {
            Some(&self.plan.destination_link)
        } else {
            Some(&self.plan.stop_links[self.stop])
        }
    }
    fn try_into_network(self: Box<Self>) -> Result<NetworkDriverAgent, LegAgent> {
        Err(self)
    }
    fn try_into_teleportation(self: Box<Self>) -> Result<TeleportationAgent, LegAgent> {
        Err(self)
    }
    fn try_into_passenger(self: Box<Self>) -> Result<PassengerAgent, LegAgent> {
        Err(self)
    }
    fn try_into_transit_driver(self: Box<Self>) -> Result<TransitDriverAgent, LegAgent> {
        Ok(self)
    }
    fn finish_leg(self: Box<Self>, _now: SimTime) -> NextPhase {
        NextPhase::Activity(Box::new(FinishedDriver {
            id: self.plan.id,
            act_type: self.plan.activity_type,
            link: self.plan.destination_link,
        }))
    }
}

impl TransitDriverLogic for PlannedDriver {
    fn vehicle(&self) -> &Id<InternalVehicle> {
        &self.plan.vehicle
    }

    fn line(&self) -> &Id<TransitLine> {
        &self.plan.line
    }

    fn route(&self) -> &Id<TransitRoute> {
        &self.plan.route
    }

    fn departure(&self) -> &Id<TransitDeparture> {
        &self.plan.departure
    }

    fn stop(&self) -> &Id<TransitStopFacility> {
        &self.plan.stops[self.stop]
    }

    fn upcoming_stops(&self) -> &[Id<TransitStopFacility>] {
        &self.plan.stops[self.stop + 1..]
    }

    fn next_time(&self) -> SimTime {
        self.next_time
    }

    fn notify_stop_arrived(&mut self, now: SimTime) {
        self.next_time = stop_times(self.plan.scheduled[self.stop], now).departure;
    }

    fn notify_stop_departed(&mut self, earliest_arrival: SimTime, _now: SimTime) {
        assert!(
            !self.upcoming_stops().is_empty(),
            "Transit driver {} cannot depart from its last stop to another stop.",
            self.id()
        );
        self.stop += 1;
        self.next_time = stop_times(self.plan.scheduled[self.stop], earliest_arrival).arrival;
    }
}

/// A driver after its departure. Like MATSim's `TransitDriverAgentImpl`, it stays in an activity
/// without end at the end link of the route. The mobsim drops it at its end, since it has no person.
struct FinishedDriver {
    id: Id<InternalPerson>,
    act_type: Id<String>,
    link: Id<Link>,
}

impl Identifiable<InternalPerson> for FinishedDriver {
    fn id(&self) -> &Id<InternalPerson> {
        &self.id
    }
}

impl SimulationAgentLogic for FinishedDriver {
    fn into_person(self: Box<Self>) -> Option<InternalPerson> {
        None
    }
}

impl ActivityLogic for FinishedDriver {
    fn act_type(&self) -> &Id<String> {
        &self.act_type
    }
    fn link_id(&self) -> &Id<Link> {
        &self.link
    }
    fn coord(&self) -> &Coordinate {
        &NO_COORDINATE
    }
    fn has_next_leg(&self) -> bool {
        false
    }
    fn has_activity_events(&self) -> bool {
        false
    }
    fn end_time(&self, _activity_start: SimTime) -> SimTime {
        SimTime::max()
    }
    fn next_wakeup(&self, _now: SimTime) -> Option<SimTime> {
        None
    }
    fn notify_started(
        &mut self,
        _comp_env: &mut ThreadLocalComputationalEnvironment,
        _now: SimTime,
    ) {
    }
    fn notify_wakeup(
        &mut self,
        _comp_env: &mut ThreadLocalComputationalEnvironment,
        _end_time: SimTime,
        _now: SimTime,
    ) {
    }
    fn notify_finished(&mut self, _now: SimTime) {}
    fn finish_activity(self: Box<Self>, _now: SimTime) -> NextPhase {
        panic!("Transit driver {} has finished its departure.", self.id)
    }
}

#[cfg(test)]
mod tests {
    use super::{PlannedDriver, WaitingDriver};
    use crate::simulation::agents::TransitDriverAgent;
    use crate::simulation::config::TransitSimulation;
    use crate::simulation::pt::schedule::{
        TransitSimSchedule, give_each_departure_its_own_vehicle,
    };
    use crate::simulation::scenario::network::Network;
    use crate::simulation::scenario::population::Population;
    use crate::simulation::scenario::transit::{DepartureIndex, TransitSchedule};
    use crate::simulation::scenario::vehicles::Garage;
    use crate::simulation::time::SimTime;
    use macros::deterministic_id_test;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    const SIX: u64 = 6 * 3600;

    /// The tutorial schedule and the 06:00 departure of route 1to3: stop 1 (departure offset 0),
    /// stop 2a (arrival 200 s, departure 240 s) and stop 3 (arrival 540 s).
    fn tutorial_departure() -> (TransitSimSchedule, DepartureIndex) {
        let network = Network::from_file("./assets/pt_tutorial/multimodalnetwork.xml");
        let mut schedule =
            TransitSchedule::from_file(Path::new("./assets/pt_tutorial/transitschedule.xml"));
        let mut garage =
            Garage::from_file(&PathBuf::from("./assets/pt_tutorial/transitVehicles.xml"));
        give_each_departure_its_own_vehicle(&mut schedule, &mut garage);
        let schedule = TransitSimSchedule::build(
            Arc::new(schedule),
            &network,
            &garage,
            &Population::default(),
            TransitSimulation::ScheduleBasedTeleportation,
        );
        let (index, _) = schedule
            .schedule()
            .indexed_departures()
            .find(|(index, _)| schedule.driver(*index).external() == "pt_Blue Line_1to3_01")
            .unwrap();
        (schedule, index)
    }

    /// The driver of the 06:00 departure of route 1to3, which it starts at `now`.
    fn driver(now: u64) -> TransitDriverAgent {
        let (schedule, index) = tutorial_departure();
        Box::new(PlannedDriver::new(
            &schedule,
            index,
            SimTime::from_secs(now),
        ))
    }

    fn arrived(driver: &mut TransitDriverAgent, now: u64) {
        driver.notify_stop_arrived(SimTime::from_secs(now));
    }

    fn departed(driver: &mut TransitDriverAgent, now: u64) {
        driver.notify_stop_departed(SimTime::from_secs(now + 1), SimTime::from_secs(now));
    }

    /// The current stop and the next time.
    fn state(driver: &TransitDriverAgent) -> (&str, u64) {
        (driver.stop().external(), driver.next_time().as_secs())
    }

    #[deterministic_id_test]
    fn driver_follows_schedule_from_stop_to_stop() {
        let driver = &mut driver(SIX);
        assert_eq!("pt_Blue Line_1to3_01", driver.id().external());
        assert_eq!("train", driver.mode().external());
        assert_eq!("Blue Line", driver.line().external());
        assert_eq!(("1", SIX), state(driver));

        arrived(driver, SIX);
        assert_eq!(("1", SIX), state(driver));
        departed(driver, SIX);
        assert_eq!(("2a", SIX + 200), state(driver));

        arrived(driver, SIX + 200);
        assert_eq!(("2a", SIX + 240), state(driver));
        departed(driver, SIX + 240);
        assert_eq!(("3", SIX + 540), state(driver));
        assert!(driver.upcoming_stops().is_empty());

        arrived(driver, SIX + 540);
        assert_eq!(("3", SIX + 540), state(driver));
        // From the last stop on, the driver is on the last link of the route.
        assert_eq!("33", driver.curr_link_id().unwrap().external());
    }

    #[deterministic_id_test]
    fn late_driver_arrives_after_earliest_arrival_and_departs_on_arrival() {
        let driver = &mut driver(SIX);
        arrived(driver, SIX);
        // Leaves stop 1 late, so that the next arrival comes later than scheduled.
        departed(driver, SIX + 300);
        assert_eq!(("2a", SIX + 301), state(driver));

        arrived(driver, SIX + 301);
        // The scheduled departure at 06:04:00 has passed, so the vehicle departs right away.
        assert_eq!(("2a", SIX + 301), state(driver));
    }

    #[deterministic_id_test]
    fn driver_started_late_arrives_at_first_stop_right_away() {
        assert_eq!(("1", SIX + 10), state(&driver(SIX + 10)));
    }

    /// Like MATSim's drivers, a driver waits in an activity until its departure, drives it as its
    /// leg and then stays in an activity without end. Its activities emit no events.
    #[deterministic_id_test]
    fn driver_waits_drives_its_departure_and_stays() {
        let (schedule, index) = tutorial_departure();
        let waiting = WaitingDriver::new_agent(Arc::new(schedule), index);
        assert_eq!("pt_Blue Line_1to3_01", waiting.id().external());
        assert_eq!("11", waiting.link_id().external());
        assert_eq!(SIX, waiting.end_time(SimTime::default()).as_secs());
        assert!(waiting.has_next_leg());
        assert!(!waiting.has_activity_events());

        let leg = waiting
            .finish_activity(SimTime::from_secs(SIX))
            .expect_leg();
        assert_eq!("11", leg.departure_link().external());
        assert_eq!(None, leg.routing_mode());
        let driver = leg.try_into_transit_driver().unwrap();
        assert_eq!("tr_1_Blue Line_1to3_01", driver.vehicle().external());
        assert_eq!(("1", SIX), state(&driver));

        let finished = driver
            .finish_leg(SimTime::from_secs(SIX + 540))
            .expect_activity();
        assert_eq!("33", finished.link_id().external());
        assert!(!finished.has_next_leg());
        assert!(!finished.has_activity_events());
        assert_eq!(None, finished.into_person());
    }
}
