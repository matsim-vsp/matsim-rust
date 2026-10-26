use crate::simulation::Identifiable;
use crate::simulation::agents::{
    AgentEvent, EndTime, EnvironmentalEventObserver, SimulationAgentLogic, SimulationAgentState,
};
use crate::simulation::id::Id;
use crate::simulation::pt::schedule::{
    DepartureIndex, SimDeparture, SimRouteStop, SimTransitRoute, TransitSimSchedule,
};
use crate::simulation::scenario::network::Link;
use crate::simulation::scenario::population::{InternalActivity, InternalLeg, InternalPerson};
use crate::simulation::time::SimTime;
use std::sync::Arc;

/// The driver of a departure of schedule-based transit, as seen by the transit engine. The driver
/// knows where the vehicle is and decides when it arrives and departs; the engine moves the
/// vehicle and lets passengers board and alight.
///
/// The engine changes the driver's state only via [`AgentEvent::TransitStopArrived`] and
/// [`AgentEvent::TransitStopDeparted`], like it notifies car drivers when they leave a link.
pub trait TransitDriver {
    fn departure_index(&self) -> DepartureIndex;
    fn departure(&self) -> &SimDeparture;
    fn route(&self) -> &SimTransitRoute;
    /// Index of [`Self::current_stop`] in the stops of [`Self::route`].
    fn stop_index(&self) -> usize;
    /// The stop at which the vehicle halts or which it approaches.
    fn current_stop(&self) -> &SimRouteStop;
    /// The stops after [`Self::current_stop`].
    fn upcoming_stops(&self) -> &[SimRouteStop];
    fn is_at_last_stop(&self) -> bool {
        self.upcoming_stops().is_empty()
    }
}

/// A driver who follows the schedule, like the drivers of SBB's deterministic transit engine.
#[derive(Debug)]
pub struct TransitDriverLogic {
    schedule: Arc<TransitSimSchedule>,
    departure: DepartureIndex,
    /// Index of the stop at which the vehicle halts or which it approaches.
    stop: usize,
    /// The next arrival while approaching a stop, the next departure while halting.
    next_time: SimTime,
}

/// Arrival and departure at a stop.
struct StopTimes {
    arrival: SimTime,
    departure: SimTime,
}

/// The single place where the times of a departure at a stop are computed. Like SBB, a vehicle
/// never arrives before `earliest_arrival` and never departs before its arrival or its scheduled
/// departure. A disruption model will hook in here.
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

impl TransitDriverLogic {
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

impl TransitDriver for TransitDriverLogic {
    fn departure_index(&self) -> DepartureIndex {
        self.departure
    }

    fn departure(&self) -> &SimDeparture {
        self.schedule.departure(self.departure)
    }

    fn route(&self) -> &SimTransitRoute {
        self.schedule.route(self.departure().route)
    }

    fn stop_index(&self) -> usize {
        self.stop
    }

    fn current_stop(&self) -> &SimRouteStop {
        &self.route().stops[self.stop]
    }

    fn upcoming_stops(&self) -> &[SimRouteStop] {
        &self.route().stops[self.stop + 1..]
    }
}

impl EndTime for TransitDriverLogic {
    fn end_time(&self, _now: SimTime) -> SimTime {
        self.next_time
    }
}

impl Identifiable<InternalPerson> for TransitDriverLogic {
    fn id(&self) -> &Id<InternalPerson> {
        &self.departure().driver
    }
}

impl EnvironmentalEventObserver for TransitDriverLogic {
    fn notify_event(&mut self, event: &mut AgentEvent, now: SimTime) {
        match event {
            AgentEvent::TransitStopArrived() => {
                self.next_time = stop_times(self.departure(), self.current_stop(), now).departure;
            }
            AgentEvent::TransitStopDeparted { earliest_arrival } => {
                if !self.is_at_last_stop() {
                    self.stop += 1;
                    self.next_time =
                        stop_times(self.departure(), self.current_stop(), *earliest_arrival)
                            .arrival;
                }
            }
            _ => {}
        }
    }
}

/// Transit drivers have no plan: they exist from the start to the end of their departure, which is
/// one leg. Methods about activities are never called for them.
impl SimulationAgentLogic for TransitDriverLogic {
    fn curr_act(&self) -> &InternalActivity {
        panic!("Transit driver {} has no activities.", self.id())
    }

    fn next_act(&self) -> &InternalActivity {
        panic!("Transit driver {} has no activities.", self.id())
    }

    fn curr_leg(&self) -> &InternalLeg {
        &self.route().driver_leg
    }

    fn next_leg(&self) -> Option<&InternalLeg> {
        None
    }

    fn advance_plan(&mut self, _now: SimTime) {
        panic!("Transit driver {} has no plan to advance.", self.id())
    }

    fn state(&self) -> SimulationAgentState {
        SimulationAgentState::LEG
    }

    fn is_wanting_to_arrive_on_current_link(&self) -> bool {
        self.is_at_last_stop()
    }

    fn curr_link_id(&self) -> Option<&Id<Link>> {
        Some(&self.current_stop().link)
    }

    fn peek_next_link_id(&self) -> Option<&Id<Link>> {
        self.upcoming_stops().first().map(|stop| &stop.link)
    }

    fn wakeup_time(&self, _now: SimTime) -> SimTime {
        panic!("Transit driver {} has no activities.", self.id())
    }

    fn as_transit_driver(&self) -> Option<&dyn TransitDriver> {
        Some(self)
    }

    fn into_person(self: Box<Self>) -> Option<InternalPerson> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::TransitDriverLogic;
    use crate::simulation::Identifiable;
    use crate::simulation::agents::agent::SimulationAgent;
    use crate::simulation::agents::{
        AgentEvent, EndTime, EnvironmentalEventObserver, SimulationAgentLogic,
    };
    use crate::simulation::id::Id;
    use crate::simulation::pt::schedule::TransitSimSchedule;
    use crate::simulation::scenario::network::{Link, Network};
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
    fn driver(now: u64) -> SimulationAgent {
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
        SimulationAgent::new(Box::new(TransitDriverLogic::new(
            schedule,
            index,
            SimTime::from_secs(now),
        )))
    }

    fn departed(agent: &mut SimulationAgent, now: u64) {
        agent.notify_event(
            &mut AgentEvent::TransitStopDeparted {
                earliest_arrival: SimTime::from_secs(now + 1),
            },
            SimTime::from_secs(now),
        );
    }

    fn arrived(agent: &mut SimulationAgent, now: u64) {
        agent.notify_event(
            &mut AgentEvent::TransitStopArrived(),
            SimTime::from_secs(now),
        );
    }

    fn stop(agent: &SimulationAgent) -> String {
        agent
            .as_transit_driver()
            .unwrap()
            .current_stop()
            .facility
            .external()
            .to_string()
    }

    #[deterministic_id_test]
    fn driver_follows_schedule_from_stop_to_stop() {
        let mut agent = driver(SIX);
        let next = |agent: &SimulationAgent| agent.end_time(SimTime::default()).as_secs();

        assert_eq!("pt_Blue Line_1to3_01", agent.id().external());
        assert_eq!(("1".to_string(), SIX), (stop(&agent), next(&agent)));
        assert_eq!(Some(&Id::<Link>::get_from_ext("11")), agent.curr_link_id());
        assert_eq!("train", agent.curr_leg().mode.external());

        arrived(&mut agent, SIX);
        assert_eq!(SIX, next(&agent));
        departed(&mut agent, SIX);
        assert_eq!(("2a".to_string(), SIX + 200), (stop(&agent), next(&agent)));

        arrived(&mut agent, SIX + 200);
        assert_eq!(SIX + 240, next(&agent));
        departed(&mut agent, SIX + 240);
        assert_eq!(("3".to_string(), SIX + 540), (stop(&agent), next(&agent)));
        assert!(agent.as_transit_driver().unwrap().is_at_last_stop());
        assert!(agent.is_wanting_to_arrive_on_current_link());
        assert!(agent.into_person().is_none());
    }

    #[deterministic_id_test]
    fn late_driver_arrives_one_tick_after_departure_and_departs_after_arrival() {
        let mut agent = driver(SIX);
        arrived(&mut agent, SIX);
        // Leaves stop 1 late, so that the next arrival comes later than scheduled.
        departed(&mut agent, SIX + 300);
        assert_eq!(SIX + 301, agent.end_time(SimTime::default()).as_secs());

        arrived(&mut agent, SIX + 301);
        // The scheduled departure at 06:04:00 has passed, so the vehicle departs right away.
        assert_eq!(SIX + 301, agent.end_time(SimTime::default()).as_secs());
    }

    #[deterministic_id_test]
    fn driver_started_late_arrives_at_first_stop_right_away() {
        let agent = driver(SIX + 10);
        assert_eq!(SIX + 10, agent.end_time(SimTime::default()).as_secs());
    }
}
