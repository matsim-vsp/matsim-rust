//! Exogenous delays of transit departures at stops of their routes, fixed before the departures
//! start. They come from an attribute of the departures ([`STOP_DELAYS_ATTRIBUTE`]), and
//! [`StopDelayDriverAgentSource`] creates the drivers which carry them out ([`StopDelayDriver`]).
//! The default [`TransitDriverAgentSource`](crate::simulation::population::agent_source::TransitDriverAgentSource)
//! ignores these attributes.

use crate::simulation::Identifiable;
use crate::simulation::agents::SimulationAgentLogic;
use crate::simulation::agents::agent::{
    LegAgent, LegLogic, NetworkDriverAgent, NextPhase, PassengerAgent, TeleportationAgent,
    TransitDriverAgent, TransitDriverLogic,
};
use crate::simulation::id::Id;
use crate::simulation::population::agent_source::{AgentSet, AgentSource};
use crate::simulation::pt::driver::{DriverStart, PlannedDriver, PlannedStart, WaitingDriver};
use crate::simulation::pt::schedule::{DepartureSequenceInRoute, TransitSimSchedule};
use crate::simulation::scenario::network::Link;
use crate::simulation::scenario::population::InternalPerson;
use crate::simulation::scenario::transit::{
    DepartureIndex, RouteIndex, StopIndexInRoute, TransitDeparture, TransitLine, TransitRoute,
    TransitSchedule, TransitStopFacility,
};
use crate::simulation::scenario::vehicles::InternalVehicle;
use crate::simulation::scenario::{MobsimScenarioPartition, PopulationShard};
use crate::simulation::time::SimTime;
use serde::Deserialize;
use serde_json::Value;
use std::time::Duration;

/// The attribute of a departure with its delays at stops of its route. Its value is a `String`
/// with a JSON array, one object per stop with delays: `stopIndex` is the position of the stop in
/// the route (from 0), `arrivalDelayOffset` is [`StopDelay::arrival`] and `departureDelayOffset`
/// is [`StopDelay::departure`]. The offsets are whole seconds, optional and 0 by default:
///
/// ```xml
/// <attribute name="delays" class="java.lang.String">[{"stopIndex":1,"arrivalDelayOffset":180},{"stopIndex":3,"arrivalDelayOffset":240,"departureDelayOffset":60}]</attribute>
/// ```
pub const STOP_DELAYS_ATTRIBUTE: &str = "delays";

/// An element of the attribute [`STOP_DELAYS_ATTRIBUTE`].
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StopDelayEntry {
    stop_index: u32,
    #[serde(default)]
    arrival_delay_offset: u32,
    #[serde(default)]
    departure_delay_offset: u32,
}

/// Exogenous delays of a departure at one stop of its route. Both delays are added to the times
/// that the driver's rules give without them, so they always count in full; the delay that the
/// vehicle carries on to the next stops is measured against the schedule, as for any other delay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StopDelay {
    /// The stop, by its position in the route.
    pub stop: StopIndexInRoute,
    /// Added to the arrival at the stop: the section before the stop takes longer, or at the first
    /// stop, the departure starts later.
    pub arrival: Duration,
    /// Added to the departure from the stop: the vehicle halts longer.
    pub departure: Duration,
}

/// The exogenous delays of a departure at the stops of its route, at most one [`StopDelay`] per
/// stop, ordered by stop.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StopDelays(Vec<StopDelay>);

impl StopDelays {
    /// The delays of `delays`, in any order.
    ///
    /// # Panics
    ///
    /// If two delays are at the same stop.
    pub fn new(delays: Vec<StopDelay>) -> Self {
        Self::sorted(delays)
            .unwrap_or_else(|stop| panic!("Two delays at the stop at position {}.", stop.index()))
    }

    /// `delays` ordered by stop, or the first stop with two delays.
    fn sorted(mut delays: Vec<StopDelay>) -> Result<Self, StopIndexInRoute> {
        delays.sort_by_key(|delay| delay.stop);
        match delays.windows(2).find(|pair| pair[0].stop == pair[1].stop) {
            Some(pair) => Err(pair[0].stop),
            None => Ok(Self(delays)),
        }
    }

    /// The delays of departure `index` from its attribute [`STOP_DELAYS_ATTRIBUTE`]. Stops whose
    /// offsets are both 0 have no delay.
    ///
    /// # Panics
    ///
    /// If the attribute is not a String with a JSON array of such objects, the offsets are not
    /// whole seconds from 0, a `stopIndex` is not a stop of the route, or two objects have the same
    /// `stopIndex`.
    pub fn of_departure(schedule: &TransitSchedule, index: DepartureIndex) -> Self {
        let departure = schedule.departure(index);
        let Some(value) = departure.attributes.get::<Value>(STOP_DELAYS_ATTRIBUTE) else {
            return Self::default();
        };
        let route = schedule.route(schedule.departure_route(index));
        let context = || {
            format!(
                "Attribute {STOP_DELAYS_ATTRIBUTE} of departure {} of transit route {} of line {}",
                departure.id, route.id, route.line
            )
        };
        let json = value.as_str().unwrap_or_else(|| {
            panic!(
                "{} must be a String with a JSON array, e.g. \
                 [{{\"stopIndex\":1,\"arrivalDelayOffset\":180}}].",
                context()
            )
        });
        let entries: Vec<StopDelayEntry> = serde_json::from_str(json).unwrap_or_else(|err| {
            panic!(
                "{} must be a JSON array of objects with stopIndex and the optional \
                 arrivalDelayOffset and departureDelayOffset in whole seconds: {err}",
                context()
            )
        });
        let delays = entries
            .into_iter()
            .map(|entry| {
                assert!(
                    (entry.stop_index as usize) < route.stops.len(),
                    "{}: stopIndex {} is not the position of a stop of the route, from 0 to {}.",
                    context(),
                    entry.stop_index,
                    route.stops.len() - 1
                );
                StopDelay {
                    stop: StopIndexInRoute::new(entry.stop_index),
                    arrival: Duration::from_secs(entry.arrival_delay_offset.into()),
                    departure: Duration::from_secs(entry.departure_delay_offset.into()),
                }
            })
            .collect();
        let delays = Self::sorted(delays).unwrap_or_else(|stop| {
            panic!(
                "{}: two objects with stopIndex {}.",
                context(),
                stop.index()
            )
        });
        Self(
            delays
                .0
                .into_iter()
                .filter(|delay| delay.arrival > Duration::ZERO || delay.departure > Duration::ZERO)
                .collect(),
        )
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The delay at `stop`, if any.
    pub fn at(&self, stop: StopIndexInRoute) -> Option<&StopDelay> {
        self.0.iter().find(|delay| delay.stop == stop)
    }
}

/// Starts a departure with a [`StopDelayDriver`] that carries out these delays.
impl DriverStart for StopDelays {
    fn start(
        self,
        schedule: &TransitSimSchedule,
        departure: DepartureIndex,
        now: SimTime,
    ) -> LegAgent {
        Box::new(StopDelayDriver::new(schedule, departure, now, self))
    }
}

/// Creates the drivers of the transit departures whose first stop is on the partition, like
/// [`TransitDriverAgentSource`](crate::simulation::population::agent_source::TransitDriverAgentSource),
/// and carries out the exogenous delays in the attributes of the departures
/// ([`STOP_DELAYS_ATTRIBUTE`]): a departure with
/// [`StopDelays`] starts with a [`StopDelayDriver`], the others with a [`PlannedDriver`]. Add it to
/// the agent sources of a controller to simulate these delays; the controller then adds no other
/// source for the transit drivers.
///
/// # Panics
///
/// When it creates the drivers, if the delay attributes of a departure are invalid, see
/// [`StopDelays::of_departure`].
pub struct StopDelayDriverAgentSource;

impl AgentSource for StopDelayDriverAgentSource {
    fn create_agents(
        &self,
        _population: &mut PopulationShard,
        partition: &MobsimScenarioPartition,
    ) -> AgentSet {
        let schedule = &partition.transit_sim_schedule;
        schedule
            .departures_starting_on(partition.rank)
            .map(|index| {
                let delays = StopDelays::of_departure(schedule.schedule(), index);
                let driver = if delays.is_empty() {
                    WaitingDriver::new_agent(schedule.clone(), index, PlannedStart)
                } else {
                    WaitingDriver::new_agent(schedule.clone(), index, delays)
                };
                (schedule.driver(index).clone(), driver)
            })
            .collect()
    }

    fn creates_transit_drivers(&self) -> bool {
        true
    }
}

/// A [`PlannedDriver`] whose departure has exogenous delays at stops of its route, fixed before the
/// departure starts ([`StopDelays`]). It follows the rules of the planned driver and adds the
/// arrival delay of a stop to the arrival at it and the departure delay to the departure from it.
/// Each is added once, since the driver reaches each stop once. The delay that the vehicle carries
/// on is measured against the schedule, so the following stops see it like any other delay, and a
/// shorter halt can absorb it.
pub struct StopDelayDriver {
    planned: PlannedDriver,
    delays: StopDelays,
}

impl StopDelayDriver {
    /// A driver who starts `departure` at `now` and approaches the first stop.
    pub fn new(
        schedule: &TransitSimSchedule,
        index: DepartureIndex,
        now: SimTime,
        delays: StopDelays,
    ) -> Self {
        let mut driver = Self {
            planned: PlannedDriver::new(schedule, index, now),
            delays,
        };
        driver.delay_arrival();
        driver
    }

    /// The delay at the stop at which the vehicle halts or which it approaches.
    fn current_delay(&self) -> Option<StopDelay> {
        let (_, stop) = self.planned.route_stop();
        self.delays.at(stop).copied()
    }

    /// Adds the arrival delay of the stop that the vehicle approaches.
    fn delay_arrival(&mut self) {
        if let Some(delay) = self.current_delay() {
            self.planned.delay_next_time(delay.arrival);
        }
    }
}

impl Identifiable<InternalPerson> for StopDelayDriver {
    fn id(&self) -> &Id<InternalPerson> {
        self.planned.id()
    }
}

impl SimulationAgentLogic for StopDelayDriver {
    fn into_person(self: Box<Self>) -> Option<InternalPerson> {
        None
    }
}

impl LegLogic for StopDelayDriver {
    fn mode(&self) -> &Id<String> {
        self.planned.mode()
    }
    fn routing_mode(&self) -> Option<&Id<String>> {
        self.planned.routing_mode()
    }
    fn departure_link(&self) -> &Id<Link> {
        self.planned.departure_link()
    }
    fn curr_link_id(&self) -> Option<&Id<Link>> {
        self.planned.curr_link_id()
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
    fn finish_leg(self: Box<Self>, now: SimTime) -> NextPhase {
        Box::new(self.planned).finish_leg(now)
    }
}

impl TransitDriverLogic for StopDelayDriver {
    fn vehicle(&self) -> &Id<InternalVehicle> {
        self.planned.vehicle()
    }

    fn line(&self) -> &Id<TransitLine> {
        self.planned.line()
    }

    fn route(&self) -> &Id<TransitRoute> {
        self.planned.route()
    }

    fn departure(&self) -> &Id<TransitDeparture> {
        self.planned.departure()
    }

    fn stop(&self) -> &Id<TransitStopFacility> {
        self.planned.stop()
    }

    fn route_stop(&self) -> (RouteIndex, StopIndexInRoute) {
        self.planned.route_stop()
    }

    fn sequence(&self) -> DepartureSequenceInRoute {
        self.planned.sequence()
    }

    fn upcoming_stops(&self) -> &[Id<TransitStopFacility>] {
        self.planned.upcoming_stops()
    }

    fn next_time(&self) -> SimTime {
        self.planned.next_time()
    }

    fn scheduled_arrival(&self) -> SimTime {
        self.planned.scheduled_arrival()
    }

    fn scheduled_departure(&self) -> SimTime {
        self.planned.scheduled_departure()
    }

    fn notify_stop_arrived(&mut self, now: SimTime) {
        self.planned.notify_stop_arrived(now);
        if let Some(delay) = self.current_delay() {
            self.planned.delay_next_time(delay.departure);
        }
    }

    fn notify_stop_departed(&mut self, earliest_arrival: SimTime, now: SimTime) {
        self.planned.notify_stop_departed(earliest_arrival, now);
        self.delay_arrival();
    }
}

#[cfg(test)]
mod tests {
    use super::{StopDelay, StopDelayDriver, StopDelayDriverAgentSource, StopDelays};
    use crate::simulation::InternalAttributes;
    use crate::simulation::agents::agent::{TransitDriverAgent, TransitDriverLogic};
    use crate::simulation::config::Config;
    use crate::simulation::io::xml::attributes::IOAttributes;
    use crate::simulation::network::sim_network::SimNetworkPartition;
    use crate::simulation::population::agent_source::AgentSource;
    use crate::simulation::pt::driver::PlannedDriver;
    use crate::simulation::pt::driver::tests::{
        SIX, arrived, departed, state, tutorial_departure_with,
    };
    use crate::simulation::pt::schedule::TransitSimSchedule;
    use crate::simulation::scenario::network::Network;
    use crate::simulation::scenario::transit::{DepartureIndex, StopIndexInRoute};
    use crate::simulation::scenario::{MobsimScenarioPartition, PopulationShard, ScenarioCore};
    use crate::simulation::time::SimTime;
    use macros::deterministic_id_test;
    use std::sync::Arc;
    use std::time::Duration;

    /// The delay at the stop at position `stop`, in seconds.
    fn delay(stop: u32, arrival: u64, departure: u64) -> StopDelay {
        StopDelay {
            stop: StopIndexInRoute::new(stop),
            arrival: Duration::from_secs(arrival),
            departure: Duration::from_secs(departure),
        }
    }

    /// The driver of the 06:00 departure of route 1to3 of the tutorial (stops 1, 2a and 3, see
    /// [`tutorial_departure_with`]), which it starts at `now`, with `delays`.
    fn delayed_driver(now: u64, delays: Vec<StopDelay>) -> TransitDriverAgent {
        let (schedule, index) = tutorial_departure_with(|_| {});
        Box::new(StopDelayDriver::new(
            &schedule,
            index,
            SimTime::from_secs(now),
            StopDelays::new(delays),
        ))
    }

    /// The tutorial departure with the attributes of the XML `attributes`.
    fn departure_with_xml(attributes: &str) -> (TransitSimSchedule, DepartureIndex) {
        let attributes: IOAttributes =
            quick_xml::de::from_str(&format!("<attributes>{attributes}</attributes>")).unwrap();
        tutorial_departure_with(|target| *target = InternalAttributes::from(attributes))
    }

    /// The delays of the tutorial departure with the attributes of the XML `attributes`.
    fn delays_of_xml(attributes: &str) -> StopDelays {
        let (schedule, index) = departure_with_xml(attributes);
        StopDelays::of_departure(schedule.schedule(), index)
    }

    /// The delays of the tutorial departure whose attribute `delays` is `json`.
    fn delays_of_json(json: &str) -> StopDelays {
        delays_of_xml(&format!(
            r#"<attribute name="delays" class="java.lang.String">{json}</attribute>"#
        ))
    }

    #[deterministic_id_test]
    fn stop_delays_come_from_the_attributes_of_the_departure() {
        let delays = delays_of_xml(
            r#"<attribute name="delays" class="java.lang.String">
                   [{"stopIndex": 2, "arrivalDelayOffset": 45},
                    {"stopIndex": 0, "arrivalDelayOffset": 120, "departureDelayOffset": 60}]
               </attribute>
               <attribute name="label" class="java.lang.String">not a delay</attribute>"#,
        );
        assert_eq!(
            StopDelays::new(vec![delay(0, 120, 60), delay(2, 45, 0)]),
            delays
        );
        assert_eq!(Some(&delay(2, 45, 0)), delays.at(StopIndexInRoute::new(2)));
        assert_eq!(None, delays.at(StopIndexInRoute::new(1)));
    }

    /// Without the attribute, with an empty array, or with offsets that are missing or 0, a
    /// departure has no delays.
    #[deterministic_id_test]
    fn departure_without_offsets_has_no_delays() {
        assert!(delays_of_xml("").is_empty());
        assert!(delays_of_json("[]").is_empty());
        assert!(
            delays_of_json(r#"[{"stopIndex":1},{"stopIndex":2,"departureDelayOffset":0}]"#)
                .is_empty()
        );
    }

    #[deterministic_id_test]
    #[should_panic(
        expected = "Attribute delays of departure 01 of transit route 1to3 of line Blue Line: \
                    stopIndex 3 is not the position of a stop of the route, from 0 to 2."
    )]
    fn stop_index_beyond_the_route_panics() {
        delays_of_json(r#"[{"stopIndex":3,"arrivalDelayOffset":60}]"#);
    }

    #[deterministic_id_test]
    #[should_panic(expected = "missing field `stopIndex`")]
    fn delay_without_stop_index_panics() {
        delays_of_json(r#"[{"arrivalDelayOffset":60}]"#);
    }

    #[deterministic_id_test]
    #[should_panic(expected = "two objects with stopIndex 1.")]
    fn two_objects_for_one_stop_panic() {
        delays_of_json(
            r#"[{"stopIndex":1,"arrivalDelayOffset":60},{"stopIndex":1,"departureDelayOffset":60}]"#,
        );
    }

    #[deterministic_id_test]
    #[should_panic(
        expected = "Attribute delays of departure 01 of transit route 1to3 of line Blue Line \
                    must be a String with a JSON array"
    )]
    fn delays_that_are_not_a_string_panic() {
        delays_of_xml(r#"<attribute name="delays" class="java.lang.Integer">60</attribute>"#);
    }

    #[deterministic_id_test]
    #[should_panic(expected = "invalid type: map, expected a sequence")]
    fn single_object_instead_of_array_panics() {
        delays_of_json(r#"{"stopIndex":1,"arrivalDelayOffset":60}"#);
    }

    #[deterministic_id_test]
    #[should_panic(expected = "unknown field `arrivalDelay`")]
    fn unknown_field_panics() {
        delays_of_json(r#"[{"stopIndex":1,"arrivalDelay":60}]"#);
    }

    #[deterministic_id_test]
    #[should_panic(expected = "invalid type: string \"00:02:00\", expected u32")]
    fn offset_as_time_of_day_panics() {
        delays_of_json(r#"[{"stopIndex":1,"departureDelayOffset":"00:02:00"}]"#);
    }

    #[deterministic_id_test]
    #[should_panic(expected = "invalid type: floating point `90.5`, expected u32")]
    fn fractional_offset_panics() {
        delays_of_json(r#"[{"stopIndex":1,"arrivalDelayOffset":90.5}]"#);
    }

    #[deterministic_id_test]
    #[should_panic(expected = "invalid value: integer `-60`, expected u32")]
    fn negative_offset_panics() {
        delays_of_json(r#"[{"stopIndex":1,"arrivalDelayOffset":-60}]"#);
    }

    /// An arrival delay at 2a lengthens the section from 1 to 2a. The halt at 2a absorbs 40 of its
    /// 60 s, the other 20 s reach stop 3, and the delay is not added again there.
    #[deterministic_id_test]
    fn arrival_delay_lengthens_the_section_before_the_stop() {
        let driver = &mut delayed_driver(SIX, vec![delay(1, 60, 0)]);
        assert_eq!(("1", SIX), state(driver));
        arrived(driver, SIX);
        assert_eq!(("1", SIX), state(driver));
        departed(driver, SIX);
        assert_eq!(("2a", SIX + 260), state(driver));

        arrived(driver, SIX + 260);
        assert_eq!(("2a", SIX + 260), state(driver));
        departed(driver, SIX + 260);
        assert_eq!(("3", SIX + 560), state(driver));
        arrived(driver, SIX + 560);
        assert_eq!(("3", SIX + 560), state(driver));
    }

    /// A departure delay at 2a lengthens the halt there, on time as well as after a late arrival,
    /// and the vehicle carries it on to stop 3.
    #[deterministic_id_test]
    fn departure_delay_lengthens_the_halt_even_after_a_late_arrival() {
        let driver = &mut delayed_driver(SIX, vec![delay(1, 0, 30)]);
        arrived(driver, SIX);
        departed(driver, SIX);
        arrived(driver, SIX + 200);
        assert_eq!(("2a", SIX + 270), state(driver));
        departed(driver, SIX + 270);
        assert_eq!(("3", SIX + 570), state(driver));

        let driver = &mut delayed_driver(SIX, vec![delay(1, 0, 30)]);
        arrived(driver, SIX);
        departed(driver, SIX + 100);
        arrived(driver, SIX + 300);
        // Departs 30 s after its late arrival, not at the scheduled departure plus 30 s.
        assert_eq!(("2a", SIX + 330), state(driver));
        departed(driver, SIX + 330);
        assert_eq!(("3", SIX + 630), state(driver));
    }

    /// Delays at the first stop postpone the start: the vehicle reaches the first stop later and
    /// halts longer there.
    #[deterministic_id_test]
    fn delays_at_the_first_stop_postpone_the_start() {
        let driver = &mut delayed_driver(SIX, vec![delay(0, 120, 60)]);
        assert_eq!(("1", SIX + 120), state(driver));
        arrived(driver, SIX + 120);
        assert_eq!(("1", SIX + 180), state(driver));
        departed(driver, SIX + 180);
        assert_eq!(("2a", SIX + 380), state(driver));
    }

    /// Both delays at one stop add up, and the halt absorbs what it can of the arrival delay only.
    #[deterministic_id_test]
    fn arrival_and_departure_delay_at_one_stop_add_up() {
        let driver = &mut delayed_driver(SIX, vec![delay(1, 60, 30)]);
        arrived(driver, SIX);
        departed(driver, SIX);
        assert_eq!(("2a", SIX + 260), state(driver));
        arrived(driver, SIX + 260);
        assert_eq!(("2a", SIX + 290), state(driver));
        departed(driver, SIX + 290);
        assert_eq!(("3", SIX + 590), state(driver));
    }

    #[deterministic_id_test]
    fn arrival_delay_at_the_last_stop_delays_only_the_end() {
        let driver = &mut delayed_driver(SIX, vec![delay(2, 45, 0)]);
        arrived(driver, SIX);
        departed(driver, SIX);
        arrived(driver, SIX + 200);
        assert_eq!(("2a", SIX + 240), state(driver));
        departed(driver, SIX + 240);
        assert_eq!(("3", SIX + 585), state(driver));
    }

    /// Delays at several stops each take effect at their stop. The 60 s of departure delay at 1
    /// reach 2a, whose halt absorbs 40 s of them; the arrival delay at 3 comes on top of the other
    /// 20 s.
    #[deterministic_id_test]
    fn delays_at_several_stops_each_take_effect_at_their_stop() {
        let driver = &mut delayed_driver(SIX, vec![delay(2, 45, 0), delay(0, 0, 60)]);
        assert_eq!(("1", SIX), state(driver));
        arrived(driver, SIX);
        assert_eq!(("1", SIX + 60), state(driver));
        departed(driver, SIX + 60);
        assert_eq!(("2a", SIX + 260), state(driver));
        arrived(driver, SIX + 260);
        assert_eq!(("2a", SIX + 260), state(driver));
        departed(driver, SIX + 260);
        assert_eq!(("3", SIX + 605), state(driver));
    }

    #[test]
    #[should_panic(expected = "Two delays at the stop at position 1.")]
    fn two_delays_at_one_stop_panic() {
        StopDelays::new(vec![delay(1, 60, 0), delay(1, 0, 30)]);
    }

    /// The agent source starts the departure with delay attributes with a [`StopDelayDriver`] and
    /// the others of the tutorial with planned drivers, which start on time.
    #[deterministic_id_test]
    fn agent_source_starts_departures_with_delays_with_stop_delay_drivers() {
        let (schedule, index) = departure_with_xml(
            r#"<attribute name="delays" class="java.lang.String">[{"stopIndex":0,"arrivalDelayOffset":120}]</attribute>"#,
        );
        let schedule = Arc::new(schedule);
        let config = Arc::new(Config::default());
        let network = Arc::new(Network::from_file(
            "./assets/pt_tutorial/multimodalnetwork.xml",
        ));
        let partition = MobsimScenarioPartition {
            rank: 0,
            network_partition: SimNetworkPartition::from_network_for_test(&network, 0, &config),
            scenario: ScenarioCore {
                network,
                config,
                ..ScenarioCore::default()
            },
            transit_sim_schedule: schedule.clone(),
        };
        let source = StopDelayDriverAgentSource;
        assert!(source.creates_transit_drivers());

        let mut agents = source.create_agents(&mut PopulationShard::default(), &partition);
        assert_eq!(
            schedule.schedule().indexed_departures().count(),
            agents.len()
        );
        for (departure, _) in schedule.schedule().indexed_departures() {
            let waiting = agents.remove(schedule.driver(departure)).unwrap();
            let start = waiting.end_time(SimTime::default());
            let driver = waiting
                .finish_activity(start)
                .expect_leg()
                .try_into_transit_driver()
                .unwrap_or_else(|_| panic!("No transit driver."));
            let planned = PlannedDriver::new(&schedule, departure, start).next_time();
            let late = if departure == index { 120 } else { 0 };
            assert_eq!(
                planned.as_secs() + late,
                driver.next_time().as_secs(),
                "{}",
                driver.id()
            );
        }
    }
}
