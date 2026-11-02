use crate::simulation::Identifiable;
use crate::simulation::agents::{SimulationAgentLogic, TransitDriverLogic};
use crate::simulation::id::Id;
use crate::simulation::pt::schedule::{
    DepartureIndex, SimDeparture, SimRouteStop, TransitSimSchedule,
};
use crate::simulation::scenario::network::Link;
use crate::simulation::scenario::population::InternalPerson;
use crate::simulation::scenario::transit::{TransitLine, TransitStopFacility};
use crate::simulation::time::SimTime;

/// Arrival and departure at a stop.
#[derive(Clone, Copy)]
struct StopTimes {
    arrival: SimTime,
    departure: SimTime,
}

/// The scheduled times of `departure` at `stop`.
fn scheduled_times(departure: &SimDeparture, stop: &SimRouteStop) -> StopTimes {
    StopTimes {
        arrival: departure.time + SimTime::from_duration(stop.arrival_offset),
        departure: departure.time + SimTime::from_duration(stop.departure_offset),
    }
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

/// The departure of a driver: what its events need and, for each stop, the facility and the
/// scheduled times.
struct DriverPlan {
    id: Id<InternalPerson>,
    mode: Id<String>,
    destination_link: Id<Link>,
    line: Id<TransitLine>,
    stops: Vec<Id<TransitStopFacility>>,
    scheduled: Vec<StopTimes>,
}

impl PlannedDriver {
    /// A driver who starts `departure` at `now` and approaches the first stop.
    pub fn new(schedule: &TransitSimSchedule, departure: DepartureIndex, now: SimTime) -> Self {
        let departure = schedule.departure(departure);
        let route = schedule.route(departure.route);
        let plan = DriverPlan {
            id: departure.driver.clone(),
            mode: route.mode.clone(),
            destination_link: route.end_link.clone(),
            line: route.line.clone(),
            stops: route.stop_facilities.clone(),
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

impl TransitDriverLogic for PlannedDriver {
    fn mode(&self) -> &Id<String> {
        &self.plan.mode
    }

    fn destination_link(&self) -> &Id<Link> {
        &self.plan.destination_link
    }

    fn line(&self) -> &Id<TransitLine> {
        &self.plan.line
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

#[cfg(test)]
mod tests {
    use super::PlannedDriver;
    use crate::simulation::agents::TransitDriverAgent;
    use crate::simulation::pt::schedule::TransitSimSchedule;
    use crate::simulation::scenario::network::Network;
    use crate::simulation::scenario::population::Population;
    use crate::simulation::scenario::transit::TransitSchedule;
    use crate::simulation::scenario::vehicles::Garage;
    use crate::simulation::time::SimTime;
    use macros::deterministic_id_test;
    use std::path::{Path, PathBuf};

    const SIX: u64 = 6 * 3600;

    /// The driver of the 06:00 departure of route 1to3: stop 1 (departure offset 0), stop 2a
    /// (arrival 200 s, departure 240 s) and stop 3 (arrival 540 s).
    fn driver(now: u64) -> TransitDriverAgent {
        let network = Network::from_file("./assets/pt_tutorial/multimodalnetwork.xml");
        let schedule =
            TransitSchedule::from_file(Path::new("./assets/pt_tutorial/transitschedule.xml"));
        let garage = Garage::from_file(&PathBuf::from("./assets/pt_tutorial/transitVehicles.xml"));
        let schedule =
            TransitSimSchedule::build(&schedule, &network, &garage, &Population::default());
        let (index, _) = schedule
            .indexed_departures()
            .find(|(_, departure)| departure.driver.external() == "pt_Blue Line_1to3_01")
            .unwrap();
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
        assert_eq!("33", driver.destination_link().external());
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
}
