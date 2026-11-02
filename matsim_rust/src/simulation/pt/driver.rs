use crate::simulation::Identifiable;
use crate::simulation::agents::{SimulationAgentLogic, TransitDriverLogic};
use crate::simulation::id::Id;
use crate::simulation::pt::schedule::{
    DepartureIndex, SimDeparture, SimRouteStop, SimTransitRoute, TransitSimSchedule,
};
use crate::simulation::scenario::population::InternalPerson;
use crate::simulation::time::SimTime;
use std::sync::Arc;

/// A driver who follows the schedule, like the drivers of SBB's deterministic transit engine.
pub struct ScheduledDriver {
    schedule: Arc<TransitSimSchedule>,
    departure: DepartureIndex,
    /// Index of the stop at which the vehicle halts or which it approaches.
    stop: usize,
    /// The arrival at the current stop while approaching it, the departure while halting there.
    next_time: SimTime,
}

/// Arrival and departure at a stop.
struct StopTimes {
    arrival: SimTime,
    departure: SimTime,
}

/// The single place where the times of a departure at a stop are computed. Like SBB, a vehicle
/// never arrives before `earliest_arrival` and never departs before its arrival or its scheduled
/// departure. Delays will hook in here.
fn stop_times(
    departure: &SimDeparture,
    stop: &SimRouteStop,
    earliest_arrival: SimTime,
) -> StopTimes {
    let arrival =
        (departure.time + SimTime::from_duration(stop.arrival_offset)).max(earliest_arrival);
    let scheduled_departure = departure.time + SimTime::from_duration(stop.departure_offset);
    StopTimes {
        arrival,
        departure: scheduled_departure.max(arrival),
    }
}

impl ScheduledDriver {
    /// A driver who starts `departure` at `now` and approaches the first stop.
    pub fn new(schedule: Arc<TransitSimSchedule>, departure: DepartureIndex, now: SimTime) -> Self {
        let mut driver = Self {
            schedule,
            departure,
            stop: 0,
            next_time: now,
        };
        driver.next_time = stop_times(driver.departure(), driver.current_stop(), now).arrival;
        driver
    }
}

impl Identifiable<InternalPerson> for ScheduledDriver {
    fn id(&self) -> &Id<InternalPerson> {
        &self.departure().driver
    }
}

impl SimulationAgentLogic for ScheduledDriver {
    fn into_person(self: Box<Self>) -> Option<InternalPerson> {
        None
    }
}

impl TransitDriverLogic for ScheduledDriver {
    fn departure_index(&self) -> DepartureIndex {
        self.departure
    }

    fn departure(&self) -> &SimDeparture {
        self.schedule.departure(self.departure)
    }

    fn route(&self) -> &SimTransitRoute {
        self.schedule.route(self.departure().route)
    }

    fn current_stop(&self) -> &SimRouteStop {
        &self.route().stops[self.stop]
    }

    fn upcoming_stops(&self) -> &[SimRouteStop] {
        &self.route().stops[self.stop + 1..]
    }

    fn next_time(&self) -> SimTime {
        self.next_time
    }

    fn notify_stop_arrived(&mut self, now: SimTime) {
        self.next_time = stop_times(self.departure(), self.current_stop(), now).departure;
    }

    fn notify_stop_departed(&mut self, earliest_arrival: SimTime, _now: SimTime) {
        assert!(
            !self.upcoming_stops().is_empty(),
            "Transit driver {} cannot depart from its last stop to another stop.",
            self.id()
        );
        self.stop += 1;
        self.next_time =
            stop_times(self.departure(), self.current_stop(), earliest_arrival).arrival;
    }
}

#[cfg(test)]
mod tests {
    use super::ScheduledDriver;
    use crate::simulation::agents::TransitDriverAgent;
    use crate::simulation::pt::schedule::TransitSimSchedule;
    use crate::simulation::scenario::network::Network;
    use crate::simulation::scenario::population::Population;
    use crate::simulation::scenario::transit::TransitSchedule;
    use crate::simulation::scenario::vehicles::Garage;
    use crate::simulation::time::SimTime;
    use macros::deterministic_id_test;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    const SIX: u64 = 6 * 3600;

    /// The driver of the 06:00 departure of route 1to3: stop 1 (departure offset 0), stop 2a
    /// (arrival 200 s, departure 240 s) and stop 3 (arrival 540 s).
    fn driver(now: u64) -> TransitDriverAgent {
        let network = Network::from_file("./assets/pt_tutorial/multimodalnetwork.xml");
        let schedule =
            TransitSchedule::from_file(Path::new("./assets/pt_tutorial/transitschedule.xml"));
        let garage = Garage::from_file(&PathBuf::from("./assets/pt_tutorial/transitVehicles.xml"));
        let schedule = Arc::new(TransitSimSchedule::build(
            &schedule,
            &network,
            &garage,
            &Population::default(),
        ));
        let (index, _) = schedule
            .indexed_departures()
            .find(|(_, departure)| departure.driver.external() == "pt_Blue Line_1to3_01")
            .unwrap();
        Box::new(ScheduledDriver::new(
            schedule,
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
        (
            driver.current_stop().facility.external(),
            driver.next_time().as_secs(),
        )
    }

    #[deterministic_id_test]
    fn driver_follows_schedule_from_stop_to_stop() {
        let mut driver = driver(SIX);

        assert_eq!("pt_Blue Line_1to3_01", driver.id().external());
        assert_eq!("train", driver.route().mode.external());
        assert_eq!(("1", SIX), state(&driver));

        arrived(&mut driver, SIX);
        assert_eq!(("1", SIX), state(&driver));
        departed(&mut driver, SIX);
        assert_eq!(("2a", SIX + 200), state(&driver));

        arrived(&mut driver, SIX + 200);
        assert_eq!(("2a", SIX + 240), state(&driver));
        departed(&mut driver, SIX + 240);
        assert_eq!(("3", SIX + 540), state(&driver));
        assert!(driver.upcoming_stops().is_empty());

        arrived(&mut driver, SIX + 540);
        assert_eq!(("3", SIX + 540), state(&driver));
        assert!(driver.into_person().is_none());
    }

    #[deterministic_id_test]
    fn late_driver_arrives_after_earliest_arrival_and_departs_on_arrival() {
        let mut driver = driver(SIX);
        arrived(&mut driver, SIX);
        // Leaves stop 1 late, so that the next arrival comes later than scheduled.
        departed(&mut driver, SIX + 300);
        assert_eq!(("2a", SIX + 301), state(&driver));

        arrived(&mut driver, SIX + 301);
        // The scheduled departure at 06:04:00 has passed, so the vehicle departs right away.
        assert_eq!(("2a", SIX + 301), state(&driver));
    }

    #[deterministic_id_test]
    fn driver_started_late_arrives_at_first_stop_right_away() {
        let driver = driver(SIX + 10);
        assert_eq!(("1", SIX + 10), state(&driver));
    }
}
