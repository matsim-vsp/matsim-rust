//! The agent driving a transit vehicle through its run, and what it does at stops.
//!
//! Port of MATSim's `TransitDriverAgentImpl` and `AbstractTransitDriverAgent`, with boarding and
//! alighting from `PassengerAccessEgressImpl`. Like the Java driver, it pretends to follow a plan
//! of `pt interaction` activities joined by `car` legs, one leg per run leg, so the network treats
//! its vehicle like any other.

use crate::simulation::Identifiable;
use crate::simulation::agents::{
    AgentEvent, EndTime, EnvironmentalEventObserver, SimulationAgentLogic, SimulationAgentState,
};
use crate::simulation::events::{
    EventsManager, PersonArrivalEventBuilder, PersonEntersVehicleEventBuilder,
    PersonLeavesVehicleEventBuilder, TransitDriverStartsEventBuilder,
    VehicleArrivesAtFacilityEventBuilder, VehicleDepartsAtFacilityEventBuilder,
};
use crate::simulation::id::Id;
use crate::simulation::pt::doors::Doors;
use crate::simulation::pt::runs::{RunLeg, ServiceRoute, VehicleRun};
use crate::simulation::pt::stops::{TransitStops, WaitingPassenger};
use crate::simulation::scenario::network::Link;
use crate::simulation::scenario::population::{
    InternalActivity, InternalGenericRoute, InternalLeg, InternalNetworkRoute, InternalPerson,
    InternalPlanElement, InternalRoute,
};
use crate::simulation::scenario::transit::TransitStopFacility;
use crate::simulation::scenario::vehicles::Garage;
use crate::simulation::time::SimTime;
use crate::simulation::vehicles::SimulationVehicle;
use std::sync::Arc;
use std::time::Duration;

/// MATSim's `PtConstants.TRANSIT_ACTIVITY_TYPE`.
pub(crate) const TRANSIT_ACTIVITY_TYPE: &str = "pt interaction";
/// MATSim's `DefaultTransitDriverAgentFactory` drives every run leg in mode `car`.
pub(crate) const DRIVER_LEG_MODE: &str = "car";
/// Line, route and departure id MATSim reports for a deadhead leg: its
/// `AbstractTransitDriverAgent.sendTransitDriverStartsEvent` emits `Id.create("Wenden", ...)`
/// for all three when the run leg has no line, which is how a deadhead piece is recognised.
pub(crate) const DEADHEAD_ID: &str = "Wenden";

/// Where a passenger on board boarded, and where they get off.
#[derive(Debug)]
struct Rider {
    boarded_at: usize,
    egress: Id<TransitStopFacility>,
}

#[derive(Debug)]
pub struct TransitDriver {
    run: Arc<VehicleRun>,
    elements: Vec<InternalPlanElement>,
    curr_element: usize,
    curr_link: usize,
    next_stop: usize,
    /// The vehicle has announced its arrival at `next_stop` and not yet departed.
    at_stop: bool,
    /// When the next leg starts. MATSim's `departureTime`: the scheduled departure, or the
    /// arrival time when the vehicle arrives late. A stop that awaits its departure time counts
    /// its offset from here.
    departure_time: SimTime,
    capacity: usize,
    doors: Doors,
    /// Parallel to the vehicle's passengers.
    riders: Vec<Rider>,
}

impl TransitDriver {
    pub(crate) fn new(run: Arc<VehicleRun>, garage: &Garage) -> Self {
        let vehicle_type = &garage.vehicle_types[&garage.vehicles[&run.vehicle].vehicle_type];
        let capacity = vehicle_type
            .capacity
            .map(|capacity| capacity.persons() as usize)
            .expect("Transit vehicle types are checked for capacity when runs are built.");

        let mut elements = Vec::with_capacity(2 * run.legs.len() + 1);
        for leg in &run.legs {
            let links = leg.links();
            elements.push(InternalPlanElement::Activity(transit_activity(&links[0])));
            let route = InternalNetworkRoute::new(
                InternalGenericRoute::new(
                    links[0].clone(),
                    links[links.len() - 1].clone(),
                    None,
                    None,
                    Some(run.vehicle.clone()),
                ),
                links.to_vec(),
            );
            elements.push(InternalPlanElement::Leg(InternalLeg::new(
                InternalRoute::Network(route),
                DRIVER_LEG_MODE,
                DRIVER_LEG_MODE,
                Duration::ZERO,
                None,
            )));
        }
        let last = run.legs.last().unwrap().links();
        elements.push(InternalPlanElement::Activity(transit_activity(
            &last[last.len() - 1],
        )));

        let RunLeg::Service { departure_time, .. } = &run.legs[0] else {
            unreachable!("A run starts with a scheduled departure.");
        };
        Self {
            departure_time: *departure_time,
            run,
            elements,
            curr_element: 0,
            curr_link: 0,
            next_stop: 0,
            at_stop: false,
            capacity,
            doors: Doors::for_vehicle_type(vehicle_type),
            riders: Vec::new(),
        }
    }

    pub(crate) fn run(&self) -> &VehicleRun {
        &self.run
    }

    pub(crate) fn next_stop_index(&self) -> usize {
        self.next_stop
    }

    pub(crate) fn next_stop_link(&self) -> &Id<Link> {
        match self.run_leg() {
            RunLeg::Service { route, .. } => &route.stops[self.next_stop].link,
            RunLeg::Deadhead { .. } => unreachable!("timetable services have no deadheads"),
        }
    }

    /// The driver has driven its last leg and stays parked for the rest of the day.
    pub(crate) fn is_finished(&self) -> bool {
        self.curr_element + 1 == self.elements.len()
    }

    fn leg_index(&self) -> usize {
        self.curr_element / 2
    }

    /// The leg the driver is on, or is about to start while it waits.
    fn run_leg(&self) -> &RunLeg {
        &self.run.legs[self.leg_index()]
    }

    /// MATSim's `sendTransitDriverStartsEvent`, sent when the driver ends its activity.
    pub(crate) fn emit_starts(&self, now: SimTime, events: &mut EventsManager) {
        let (line, route, departure) = match self.run_leg() {
            RunLeg::Service {
                route, departure, ..
            } => (route.line.clone(), route.route.clone(), departure.clone()),
            RunLeg::Deadhead { .. } => (
                Id::get_from_ext(DEADHEAD_ID),
                Id::get_from_ext(DEADHEAD_ID),
                Id::get_from_ext(DEADHEAD_ID),
            ),
        };
        events.process_event(
            &TransitDriverStartsEventBuilder::default()
                .time(now)
                .driver(self.run.driver.clone())
                .vehicle(self.run.vehicle.clone())
                .line(line)
                .route(route)
                .departure(departure)
                .build()
                .unwrap(),
        );
    }

    /// Seconds the vehicle has to wait so it does not leave a stop that awaits its departure
    /// time early. MATSim's `longerStopTimeIfWeAreAheadOfSchedule`.
    fn wait_for_schedule(
        &self,
        stop_departure: Option<Duration>,
        awaits: bool,
        now: SimTime,
    ) -> f64 {
        match stop_departure {
            Some(offset) if awaits => {
                let earliest = self.departure_time.saturating_add(offset);
                if now < earliest {
                    earliest.duration_since(now).as_secs_f64()
                } else {
                    0.0
                }
            }
            _ => 0.0,
        }
    }
}

fn transit_activity(link: &Id<Link>) -> InternalActivity {
    InternalActivity::new(None, TRANSIT_ACTIVITY_TYPE, link.clone(), None, None, None)
}

/// Seconds the vehicle still has to stand at the stop before it may leave, zero when the stop
/// declares no minimum dwell or the vehicle has already stood there long enough.
/// Seconds between `now` and the scheduled time of the stop, negative when early.
fn delay(now: SimTime, departure: SimTime, offset: Option<Duration>) -> f64 {
    let scheduled = departure.as_nanos() as i128 + offset.map_or(0, |o| o.as_nanos() as i128);
    (now.as_nanos() as i128 - scheduled) as f64 / 1e9
}

#[derive(Debug, PartialEq)]
pub(crate) enum StopOutcome {
    /// The vehicle's next stop is not on this link, so it drives on.
    NoStop,
    /// The vehicle has left the stop; its next stop may lie on the same link.
    Departed,
    /// The vehicle stays this many seconds and is asked again afterwards.
    Dwell { seconds: f64, blocks_lane: bool },
}

/// Serves the vehicle's next stop if it lies on `link`, the link the vehicle is about to leave.
/// MATSim's `TransitQLink.handleTransitStop` and `AbstractTransitDriverAgent.handleTransitStop`.
pub(crate) fn serve_stop(
    vehicle: &mut SimulationVehicle,
    link: &Id<Link>,
    now: SimTime,
    stops: &mut TransitStops,
    events: &mut EventsManager,
    timetable: bool,
) -> StopOutcome {
    let Some((driver, passengers, vehicle_id)) = vehicle.transit_parts_mut() else {
        return StopOutcome::NoStop;
    };
    let RunLeg::Service {
        route,
        departure_time: scheduled,
        ..
    } = driver.run_leg()
    else {
        return StopOutcome::NoStop;
    };
    let route: Arc<ServiceRoute> = route.clone();
    let scheduled = *scheduled;
    let Some(stop) = route.stops.get(driver.next_stop) else {
        return StopOutcome::NoStop;
    };
    if &stop.link != link {
        return StopOutcome::NoStop;
    }

    if !driver.at_stop {
        driver.at_stop = true;
        events.process_event(
            &VehicleArrivesAtFacilityEventBuilder::default()
                .time(now)
                .vehicle(vehicle_id.clone())
                .facility(stop.facility.clone())
                .delay(delay(
                    now,
                    scheduled,
                    stop.arrival_offset.or(stop.departure_offset),
                ))
                .build()
                .unwrap(),
        );
    }

    let leaving: Vec<usize> = driver
        .riders
        .iter()
        .enumerate()
        .filter(|(_, rider)| stop.allow_alighting && rider.egress == stop.facility)
        .map(|(position, _)| position)
        .collect();
    let mut free = driver.capacity - passengers.len() + leaving.len();
    let mut entering = Vec::new();
    for (position, waiting) in stops.waiting_at(&stop.facility).iter().enumerate() {
        if free == 0 {
            break;
        }
        if !stop.allow_boarding {
            break;
        }
        let stops_to_come = route.stops[driver.next_stop + 1..]
            .iter()
            .map(|stop| stop.facility.clone());
        if waiting.accepts(&route.line, stops_to_come) {
            entering.push(position);
            free -= 1;
        }
    }

    let step = driver.doors.step(leaving.len(), entering.len());
    // Parallel doors board before they alight, as MATSim's handler does; serial doors only do
    // one of the two in a call.
    for WaitingPassenger { agent, egress, .. } in
        stops.take(&stop.facility, &entering[..step.board])
    {
        events.process_event(
            &PersonEntersVehicleEventBuilder::default()
                .time(now)
                .person(agent.id().clone())
                .vehicle(vehicle_id.clone())
                .build()
                .unwrap(),
        );
        passengers.push(agent);
        driver.riders.push(Rider {
            boarded_at: driver.next_stop,
            egress,
        });
    }
    for &position in leaving[..step.alight].iter().rev() {
        let rider = driver.riders.remove(position);
        let mut agent = passengers.remove(position);
        let access_link = &route.stops[rider.boarded_at].link;
        let mut leaves = PersonLeavesVehicleEventBuilder::default()
            .time(now)
            .person(agent.id().clone())
            .vehicle(vehicle_id.clone())
            .build()
            .unwrap();
        // The experienced-plan collector rebuilds the ride from these; event files never carry
        // them, as MATSim's PersonLeavesVehicle has no such attributes.
        leaves.attributes.insert(RIDE_LINE, route.line.external());
        leaves.attributes.insert(RIDE_ROUTE, route.route.external());
        leaves
            .attributes
            .insert(RIDE_DISTANCE, route.ride_distance(access_link, &stop.link));
        events.process_event(&leaves);

        agent.notify_event(&mut AgentEvent::LeftTransitVehicle(), now);
        events.process_event(
            &PersonArrivalEventBuilder::default()
                .time(now)
                .person(agent.id().clone())
                .link(agent.curr_link_id().unwrap().clone())
                .leg_mode(agent.curr_leg().mode.clone())
                .build()
                .unwrap(),
        );
        stops.push_alighted(agent);
    }

    let mut stop_time = step.stop_time;
    if stop_time == 0.0 {
        let offset = if timetable {
            stop.departure_offset.or(stop.arrival_offset)
        } else {
            stop.departure_offset
        };
        stop_time = driver.wait_for_schedule(offset, timetable || stop.await_departure, now);
    }

    if stop_time == 0.0 {
        events.process_event(
            &VehicleDepartsAtFacilityEventBuilder::default()
                .time(now)
                .vehicle(vehicle_id.clone())
                .facility(stop.facility.clone())
                .delay(delay(
                    now,
                    scheduled,
                    stop.departure_offset.or(stop.arrival_offset),
                ))
                .build()
                .unwrap(),
        );
        driver.next_stop += 1;
        driver.at_stop = false;
        assert!(
            driver.next_stop < route.stops.len() || passengers.is_empty(),
            "Transit vehicle {vehicle_id} left its last stop with passengers on board."
        );
        return StopOutcome::Departed;
    }
    StopOutcome::Dwell {
        seconds: stop_time,
        blocks_lane: stop.is_blocking,
    }
}

/// Attributes of an in-memory `PersonLeavesVehicleEvent` that describe a transit ride.
pub(crate) const RIDE_LINE: &str = "transitLine";
pub(crate) const RIDE_ROUTE: &str = "transitRoute";
pub(crate) const RIDE_DISTANCE: &str = "rideDistance";

impl Identifiable<InternalPerson> for TransitDriver {
    fn id(&self) -> &Id<InternalPerson> {
        &self.run.driver
    }
}

impl EnvironmentalEventObserver for TransitDriver {
    fn notify_event(&mut self, event: &mut AgentEvent, _now: SimTime) {
        if let AgentEvent::LeftLink() = event {
            self.curr_link += 1;
        }
    }
}

impl EndTime for TransitDriver {
    fn end_time(&self, _now: SimTime) -> SimTime {
        self.departure_time
    }
}

impl SimulationAgentLogic for TransitDriver {
    fn curr_act(&self) -> &InternalActivity {
        self.elements[self.curr_element].as_activity().unwrap()
    }

    fn next_act(&self) -> &InternalActivity {
        let next = self.curr_element
            + if self.curr_element.is_multiple_of(2) {
                2
            } else {
                1
            };
        self.elements[next].as_activity().unwrap()
    }

    fn curr_leg(&self) -> &InternalLeg {
        self.elements[self.curr_element].as_leg().unwrap()
    }

    fn next_leg(&self) -> Option<&InternalLeg> {
        let next = self.curr_element
            + if self.curr_element.is_multiple_of(2) {
                1
            } else {
                2
            };
        self.elements
            .get(next)
            .and_then(InternalPlanElement::as_leg)
    }

    fn advance_plan(&mut self, now: SimTime) {
        self.curr_element += 1;
        self.curr_link = 0;
        if self.state() == SimulationAgentState::LEG {
            self.next_stop = 0;
            self.at_stop = false;
            return;
        }
        // MATSim's `endLegAndComputeNextState`: a scheduled leg starts at its departure time, a
        // deadhead right away, and a late vehicle as soon as it arrives.
        if self.is_finished() {
            self.departure_time = SimTime::max();
            return;
        }
        if let RunLeg::Service { departure_time, .. } = self.run_leg() {
            self.departure_time = *departure_time;
        }
        if self.departure_time < now {
            self.departure_time = now;
        }
    }

    fn state(&self) -> SimulationAgentState {
        if self.curr_element.is_multiple_of(2) {
            SimulationAgentState::ACTIVITY
        } else {
            SimulationAgentState::LEG
        }
    }

    fn is_wanting_to_arrive_on_current_link(&self) -> bool {
        self.peek_next_link_id().is_none()
    }

    fn curr_link_id(&self) -> Option<&Id<Link>> {
        if self.state() != SimulationAgentState::LEG {
            return None;
        }
        self.run_leg().links().get(self.curr_link)
    }

    fn peek_next_link_id(&self) -> Option<&Id<Link>> {
        self.run_leg().links().get(self.curr_link + 1)
    }

    fn wakeup_time(&self, _now: SimTime) -> SimTime {
        self.departure_time
    }

    fn into_person(self: Box<Self>) -> Option<InternalPerson> {
        None
    }

    fn transit_driver(&self) -> Option<&TransitDriver> {
        Some(self)
    }

    fn transit_driver_mut(&mut self) -> Option<&mut TransitDriver> {
        Some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simulation::pt::runs::TransitVehicleRuns;
    use crate::simulation::scenario::network::Network;
    use crate::simulation::scenario::transit::TransitSchedule;
    use macros::deterministic_id_test;
    use std::path::PathBuf;

    fn tutorial_driver() -> TransitDriver {
        let schedule =
            TransitSchedule::from_file(&PathBuf::from("./assets/pt_tutorial/transitschedule.xml"));
        let garage = Garage::from_file(&PathBuf::from("./assets/pt_tutorial/transitVehicles.xml"));
        let network =
            Network::from_file_as_is(&PathBuf::from("./assets/pt_tutorial/multimodalnetwork.xml"));
        let runs = TransitVehicleRuns::build(&schedule, &garage, &network, &[]).unwrap();
        TransitDriver::new(runs.runs()[0].clone(), &garage)
    }

    #[deterministic_id_test]
    fn driver_follows_its_run_leg_by_leg() {
        let mut driver = tutorial_driver();
        assert_eq!(SimulationAgentState::ACTIVITY, driver.state());
        assert_eq!(
            SimTime::from_secs(21600),
            driver.wakeup_time(SimTime::default())
        );
        assert_eq!(None, driver.curr_link_id());

        driver.advance_plan(SimTime::from_secs(21600));
        assert_eq!(SimulationAgentState::LEG, driver.state());
        assert_eq!("car", driver.curr_leg().mode.external());
        let mut links = vec![driver.curr_link_id().unwrap().external().to_owned()];
        while !driver.is_wanting_to_arrive_on_current_link() {
            driver.notify_event(&mut AgentEvent::LeftLink(), SimTime::default());
            links.push(driver.curr_link_id().unwrap().external().to_owned());
        }
        assert_eq!(vec!["11", "12", "23", "33"], links);

        // Arriving before the next departure waits for it; arriving late starts right away.
        driver.advance_plan(SimTime::from_secs(22104));
        assert_eq!(
            SimTime::from_secs(22500),
            driver.end_time(SimTime::default())
        );
        driver.advance_plan(SimTime::from_secs(22500));
        driver.advance_plan(SimTime::from_secs(23500));
        assert_eq!(
            SimTime::from_secs(23500),
            driver.end_time(SimTime::default())
        );
    }

    #[deterministic_id_test]
    fn finished_driver_never_wakes_up() {
        let mut driver = tutorial_driver();
        let legs = driver.run().legs.len();
        for _ in 0..2 * legs {
            driver.advance_plan(SimTime::from_secs(0));
        }
        assert!(driver.is_finished());
        assert!(driver.next_leg().is_none());
        assert_eq!(SimTime::max(), driver.wakeup_time(SimTime::default()));
    }

    #[test]
    fn delay_is_negative_when_early() {
        let departure = SimTime::from_secs(21600);
        assert_eq!(
            1.0,
            delay(
                SimTime::from_secs(21801),
                departure,
                Some(Duration::from_secs(200))
            )
        );
        assert_eq!(
            -39.0,
            delay(
                SimTime::from_secs(21801),
                departure,
                Some(Duration::from_secs(240))
            )
        );
        assert_eq!(0.0, delay(departure, departure, None));
    }
}
