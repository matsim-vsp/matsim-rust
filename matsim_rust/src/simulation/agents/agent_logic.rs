use crate::external_services::ExternalServiceType;
use crate::external_services::routing::{
    InternalRoutingRequest, InternalRoutingRequestPayloadBuilder, InternalRoutingResponse,
};
use crate::simulation::Identifiable;
use crate::simulation::agents::{
    ActivityLogic, ActivityMut, AgentPhase, AgentPhaseMut, LegLogic, LegMut, NetworkDriverLogic,
    SimulationAgentLogic, TeleportationLogic, TransitTeleportation,
};
use crate::simulation::controller::ThreadLocalComputationalEnvironment;
use crate::simulation::id::Id;
use crate::simulation::replanning::routing::Facility;
use crate::simulation::scenario::network::Link;
use crate::simulation::scenario::population::{
    InternalActivity, InternalLeg, InternalPerson, InternalPlanElement, InternalRoute,
};
use crate::simulation::scenario::trip_structure_utils::{
    find_trip_span_starting_at_activity_default, identify_main_mode,
};
use crate::simulation::scenario::vehicles::InternalVehicle;
use crate::simulation::scenario::{Coordinate, ScenarioCore};
use crate::simulation::time::SimTime;
use std::fmt::{Debug, Formatter};
use std::time::Duration;
use tokio::sync::mpsc::Sender;
use tokio::sync::oneshot::Receiver;
use tracing::trace;

#[derive(Debug, PartialEq, Clone)]
pub struct PlanBasedSimulationLogic {
    inner: PlanLogic,
}

// Roles are implemented only on private cores. A public facade exposes them through its phase.
#[derive(Debug, PartialEq, Clone)]
struct PlanLogic {
    basic_agent_delegate: InternalPerson,
    selected_plan: usize,
    curr_plan_element: usize,
    curr_route_element: usize,
    activity_end_time: Option<SimTime>,
    // Set when a leg starts. Within-day replanning replaces only legs that have not started yet,
    // so the route of the current leg cannot change before the next transition.
    has_network_route: bool,
}

#[derive(Debug)]
pub struct AdaptivePlanBasedSimulationLogic {
    inner: AdaptiveLogic,
}

struct AdaptiveLogic {
    delegate: PlanLogic,
    route_receiver: Option<Receiver<InternalRoutingResponse>>,
}

impl Debug for AdaptiveLogic {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:?}, RouteReceiver {:?}",
            self.delegate,
            self.route_receiver.is_some()
        )
    }
}

impl PlanBasedSimulationLogic {
    pub fn new(person: InternalPerson) -> Self {
        Self {
            inner: PlanLogic::new(person),
        }
    }
}

impl Identifiable<InternalPerson> for PlanBasedSimulationLogic {
    fn id(&self) -> &Id<InternalPerson> {
        self.inner.id()
    }
}

impl SimulationAgentLogic for PlanBasedSimulationLogic {
    fn phase(&self) -> AgentPhase<'_> {
        self.inner.phase()
    }

    fn phase_mut(&mut self) -> AgentPhaseMut<'_> {
        self.inner.phase_mut()
    }

    fn into_person(self: Box<Self>) -> InternalPerson {
        self.inner.into_person()
    }
}

impl Identifiable<InternalPerson> for PlanLogic {
    fn id(&self) -> &Id<InternalPerson> {
        self.basic_agent_delegate.id()
    }
}

impl PlanLogic {
    fn new(basic_agent_delegate: InternalPerson) -> Self {
        // The selected plan is fixed during mobsim; adaptive routing only replaces its elements.
        let selected_plan = basic_agent_delegate
            .plans()
            .iter()
            .position(|plan| plan.selected)
            .expect("Simulation agent must have a selected plan");
        let first_act_end = basic_agent_delegate.plans()[selected_plan].elements[0]
            .as_activity()
            .unwrap()
            .cmp_end_time(SimTime::default());
        Self {
            basic_agent_delegate,
            selected_plan,
            curr_plan_element: 0,
            curr_route_element: 0,
            activity_end_time: Some(first_act_end),
            has_network_route: false,
        }
    }

    fn plan_element_at(&self, index: usize) -> Option<&InternalPlanElement> {
        self.basic_agent_delegate.plans()[self.selected_plan]
            .elements
            .get(index)
    }

    fn is_activity(&self) -> bool {
        self.curr_plan_element.is_multiple_of(2)
    }

    fn phase(&self) -> AgentPhase<'_> {
        if self.is_activity() {
            AgentPhase::Activity(self)
        } else {
            AgentPhase::Leg(self)
        }
    }

    fn phase_mut(&mut self) -> AgentPhaseMut<'_> {
        let id = self.id().internal();
        if self.is_activity() {
            AgentPhaseMut::Activity(ActivityMut::new(self, id))
        } else {
            AgentPhaseMut::Leg(LegMut::new(self, id))
        }
    }

    fn into_person(self) -> InternalPerson {
        self.basic_agent_delegate
    }

    fn advance(&mut self, now: SimTime) {
        self.curr_plan_element += 1;
        self.curr_route_element = 0;
        assert!(
            self.curr_plan_element < self.basic_agent_delegate.total_elements(),
            "Cannot advance plan of agents {:?} beyond its last element.",
            self.id()
        );
        // Every plan element remains in the person. Transitions only change the cursors and
        // cached activity time/capability; materialization needs no restoration step.
        if self.is_activity() {
            self.activity_end_time = Some(self.curr_act().cmp_end_time(now));
            self.has_network_route = false;
        } else {
            self.activity_end_time = None;
            self.has_network_route = self
                .curr_leg()
                .route
                .as_ref()
                .is_some_and(|route| matches!(route, InternalRoute::Network(_)));
        }
    }

    fn curr_act(&self) -> &InternalActivity {
        self.plan_element_at(self.curr_plan_element)
            .and_then(|p| p.as_activity())
            .expect("Activity phase must point to an activity")
    }

    fn next_leg(&self) -> Option<&InternalLeg> {
        // The next plan element after the current activity should be a leg.
        self.plan_element_at(self.curr_plan_element + 1)
            .and_then(|p| p.as_leg())
    }

    fn curr_leg(&self) -> &InternalLeg {
        self.plan_element_at(self.curr_plan_element)
            .and_then(|p| p.as_leg())
            .expect("Leg phase must point to a leg")
    }

    fn route(&self) -> &InternalRoute {
        self.curr_leg()
            .route
            .as_ref()
            .unwrap_or_else(|| panic!("Missing route for agent {}", self.id()))
    }

    fn current_link(&self) -> Option<&Id<Link>> {
        match self.route() {
            InternalRoute::Network(n) => n.route_element_at(self.curr_route_element),
            route => match self.curr_route_element {
                0 => Some(route.start_link()),
                1 => Some(route.end_link()),
                _ => panic!(
                    "A generic route only has two elements. Current plan element {:?}, Current route element {:?}, Current agent {:?}",
                    self.curr_plan_element,
                    self.curr_route_element,
                    self.id()
                ),
            },
        }
    }

    /// This method advances the pointer to the last element of the route. We need this in case of
    /// teleported legs. Advancing the route pointer to the last element directly ensures that teleporting
    /// the vehicle is independent of whether the leg has a Generic-Teleportation route or a network
    /// route.
    fn set_curr_route_element_to_last(&mut self) {
        self.curr_route_element = match self.route() {
            InternalRoute::Network(n) => n.route().len() - 1,
            _ => 1,
        };
    }
}

impl ActivityLogic for PlanLogic {
    fn act_type(&self) -> &Id<String> {
        &self.curr_act().act_type
    }
    fn link_id(&self) -> &Id<Link> {
        self.curr_act().link_id()
    }
    fn coord(&self) -> &Coordinate {
        self.curr_act().coord()
    }
    fn has_next_leg(&self) -> bool {
        self.next_leg().is_some()
    }
    fn end_time(&self, _activity_start: SimTime) -> SimTime {
        // The end time is fixed when the activity starts, see `advance`.
        self.activity_end_time.unwrap()
    }
    fn wakeup_time(&self, _: SimTime) -> SimTime {
        self.activity_end_time.unwrap()
    }
    fn on_started(&mut self, _comp_env: &mut ThreadLocalComputationalEnvironment, _now: SimTime) {}
    fn on_wakeup(
        &mut self,
        _comp_env: &mut ThreadLocalComputationalEnvironment,
        _end_time: SimTime,
        _now: SimTime,
    ) {
    }
    fn on_finished(&mut self, _now: SimTime) {}
    fn start_next_leg(&mut self, now: SimTime) {
        self.advance(now);
    }
}

impl LegLogic for PlanLogic {
    fn mode(&self) -> &Id<String> {
        &self.curr_leg().mode
    }
    fn routing_mode(&self) -> Option<&Id<String>> {
        self.curr_leg().routing_mode.as_ref()
    }
    fn departure_link(&self) -> &Id<Link> {
        self.route().start_link()
    }
    fn curr_link_id(&self) -> Option<&Id<Link>> {
        self.current_link()
    }
    fn network_driver(&self) -> Option<&dyn NetworkDriverLogic> {
        self.has_network_route.then_some(self)
    }
    fn network_driver_mut(&mut self) -> Option<&mut dyn NetworkDriverLogic> {
        if self.has_network_route {
            Some(self)
        } else {
            None
        }
    }
    fn teleportation(&self) -> Option<&dyn TeleportationLogic> {
        Some(self)
    }
    fn teleportation_mut(&mut self) -> Option<&mut dyn TeleportationLogic> {
        Some(self)
    }
    fn start_next_activity(&mut self, now: SimTime) {
        self.advance(now);
    }
}

impl NetworkDriverLogic for PlanLogic {
    fn planned_vehicle_id(&self) -> Option<&Id<InternalVehicle>> {
        self.route().as_generic().vehicle().as_ref()
    }
    fn peek_next_link_id(&self) -> Option<&Id<Link>> {
        self.route()
            .as_network()
            .expect("Network-driver role must have a network route")
            .route_element_at(self.curr_route_element + 1)
    }
    fn is_wanting_to_arrive_on_current_link(&self) -> bool {
        self.peek_next_link_id().is_none()
    }
    fn move_over_node(&mut self, next_link: &Id<Link>, _now: SimTime) {
        debug_assert_eq!(
            self.peek_next_link_id(),
            Some(next_link),
            "Agent {} must move onto the link it chose",
            self.id()
        );
        self.curr_route_element += 1;
    }
}

impl TeleportationLogic for PlanLogic {
    fn destination_link(&self) -> &Id<Link> {
        // Network routes contain both endpoints. Teleportation advances to the vector's last
        // element, so partition transport must use that same link rather than route metadata.
        match self.route() {
            InternalRoute::Network(route) => route
                .route()
                .last()
                .expect("Network route must not be empty"),
            route => route.end_link(),
        }
    }
    fn arrival_time(&self, departure: SimTime) -> SimTime {
        departure.saturating_add(self.curr_leg().travel_time())
    }
    fn distance(&self) -> Option<f64> {
        self.route().as_generic().distance()
    }
    fn transit(&self) -> Option<TransitTeleportation<'_>> {
        self.route().as_pt().map(|pt| {
            let description = &pt.description;
            TransitTeleportation {
                line: &description.transit_line_id,
                route: &description.transit_route_id,
                boarding_time: description.boarding_time,
                access_facility: &description.access_facility_id,
                egress_facility: &description.egress_facility_id,
            }
        })
    }
    fn begin_teleportation(&mut self, _now: SimTime) {
        self.set_curr_route_element_to_last();
    }
}

impl AdaptivePlanBasedSimulationLogic {
    pub fn new(person: InternalPerson) -> Self {
        Self {
            inner: AdaptiveLogic {
                delegate: PlanLogic::new(person),
                route_receiver: None,
            },
        }
    }
}

impl Identifiable<InternalPerson> for AdaptivePlanBasedSimulationLogic {
    fn id(&self) -> &Id<InternalPerson> {
        self.inner.delegate.id()
    }
}

impl SimulationAgentLogic for AdaptivePlanBasedSimulationLogic {
    fn phase(&self) -> AgentPhase<'_> {
        if self.inner.delegate.is_activity() {
            AgentPhase::Activity(&self.inner)
        } else {
            self.inner.delegate.phase()
        }
    }

    fn phase_mut(&mut self) -> AgentPhaseMut<'_> {
        if self.inner.delegate.is_activity() {
            let id = self.inner.delegate.id().internal();
            AgentPhaseMut::Activity(ActivityMut::new(&mut self.inner, id))
        } else {
            self.inner.delegate.phase_mut()
        }
    }

    fn into_person(self: Box<Self>) -> InternalPerson {
        self.inner.delegate.into_person()
    }
}

impl ActivityLogic for AdaptiveLogic {
    fn act_type(&self) -> &Id<String> {
        self.delegate.act_type()
    }
    fn link_id(&self) -> &Id<Link> {
        self.delegate.link_id()
    }
    fn coord(&self) -> &Coordinate {
        self.delegate.coord()
    }
    fn has_next_leg(&self) -> bool {
        self.delegate.has_next_leg()
    }
    fn end_time(&self, activity_start: SimTime) -> SimTime {
        self.delegate.end_time(activity_start)
    }

    fn wakeup_time(&self, now: SimTime) -> SimTime {
        let mut end = self.delegate.wakeup_time(now);
        if self.delegate.next_leg().is_none() {
            // no need to wake up if there is no other leg.
            return end;
        }
        let horizon: Option<u32> = self
            .delegate
            .curr_act()
            .attributes
            .get(crate::simulation::scenario::population::PREPLANNING_HORIZON);
        if let Some(h) = horizon {
            if SimTime::from_secs(h as u64) > end {
                // if horizon is larger than the current end time, then end - h would be negative (might be the case at the very beginning of the simulation)
                // and thus there would be an error.
                end = SimTime::default();
            } else {
                end = end.saturating_sub(Duration::from_secs(h as u64));
            }
        }
        end
    }

    // The adaptive logic reacts first and then lets the plan logic handle the same callback.
    fn on_started(&mut self, comp_env: &mut ThreadLocalComputationalEnvironment, now: SimTime) {
        self.delegate.on_started(comp_env, now);
    }
    fn on_wakeup(
        &mut self,
        comp_env: &mut ThreadLocalComputationalEnvironment,
        end_time: SimTime,
        now: SimTime,
    ) {
        self.react_to_woke_up(comp_env, end_time, now);
        self.delegate.on_wakeup(comp_env, end_time, now);
    }
    fn on_finished(&mut self, now: SimTime) {
        self.replace_route(now);
        self.delegate.on_finished(now);
    }
    fn start_next_leg(&mut self, now: SimTime) {
        self.delegate.start_next_leg(now);
    }
}

impl AdaptiveLogic {
    fn react_to_woke_up(
        &mut self,
        comp_env: &mut ThreadLocalComputationalEnvironment,
        departure_time: SimTime,
        now: SimTime,
    ) {
        if self.route_receiver.is_some() {
            // If we already have a route request in progress, we do not call the router again.
            return;
        }

        let preplan = self.delegate.next_leg().is_some()
            && self
                .delegate
                .curr_act()
                .attributes
                .get::<u32>(crate::simulation::scenario::population::PREPLANNING_HORIZON)
                .is_some();

        if !preplan {
            // No reason to call the router if we are not preplanning.
            return;
        }

        self.call_router(comp_env, departure_time, now);
    }

    #[tracing::instrument(level = "trace", skip(comp_env), fields(uuid = tracing::field::Empty, person_id = self.delegate.id().external(), mode = tracing::field::Empty))]
    fn call_router(
        &mut self,
        comp_env: &mut ThreadLocalComputationalEnvironment,
        departure_time: SimTime,
        now: SimTime,
    ) {
        let (send, recv) = tokio::sync::oneshot::channel();

        let trip_span = find_trip_span_starting_at_activity_default(
            &self
                .delegate
                .basic_agent_delegate
                .selected_plan()
                .unwrap()
                .elements,
            self.delegate.curr_plan_element,
        )
        .unwrap_or_else(|| {
            panic!(
                "No trip found for agent {:?} at plan element {:?} at time {:?}",
                self.delegate.id(),
                self.delegate.curr_plan_element,
                now
            )
        });

        let plan_elements = &self
            .delegate
            .basic_agent_delegate
            .selected_plan()
            .unwrap()
            .elements;
        let origin = trip_span.origin(plan_elements);
        let destination = trip_span.destination(plan_elements);

        let mode =
            identify_main_mode(trip_span.trip_elements(plan_elements)).unwrap_or_else(|| {
                panic!(
                    "Could not identify main mode for trip starting at activity {:?} in agent {:?}",
                    origin,
                    self.delegate.id()
                )
            });

        let scenario_core = comp_env.scenario_core();
        let (from_link, from) = routing_location(scenario_core, origin, &mode);
        let (to_link, to) = routing_location(scenario_core, destination, &mode);

        let payload = InternalRoutingRequestPayloadBuilder::default()
            .person_id(self.delegate.id().external().to_string())
            .from_link(from_link)
            .from(from)
            .to_link(to_link)
            .to(to)
            .mode(mode.clone())
            .departure_time(departure_time)
            .now(now)
            .build()
            .unwrap();

        trace!(uuid = payload.uuid.as_u128(), mode = mode.as_str());

        let request = InternalRoutingRequest {
            payload,
            response_tx: send,
        };

        comp_env
            .get_service::<Sender<InternalRoutingRequest>>(ExternalServiceType::Routing(mode.clone()))
            .unwrap_or_else(|| panic!("There is not service registered for routing of mode {} and agent id {}. Please make sure that you have started a corresponding thread. Next leg {:?}", mode, self.delegate.id(), self.delegate.next_leg()))
            .blocking_send(request)
            .expect("InternalRoutingRequest channel closed unexpectedly");

        self.route_receiver = Some(recv);
    }

    #[tracing::instrument(level = "trace", fields(person_id = self.delegate.id().external()))]
    fn replace_route(&mut self, _now: SimTime) {
        if self.route_receiver.is_none() {
            // No route request in progress, nothing to replace.
            return;
        }

        let response = self.blocking_recv(_now);

        trace!(uuid = response.request_id.as_u128());

        self.replace_next_trip(response, _now);
    }

    #[tracing::instrument(level = "trace", fields(person_id = self.delegate.id().external()))]
    fn blocking_recv(&mut self, _now: SimTime) -> InternalRoutingResponse {
        let receiver = self.route_receiver.take().unwrap();
        let response = receiver
            .blocking_recv()
            .expect("InternalRoutingRequest channel closed unexpectedly");

        trace!(uuid = response.request_id.as_u128());

        response
    }

    /// Replaces the next trip in the plan with the legs and activities from the given InternalRoutingResponse.
    #[tracing::instrument(level = "trace", skip(response), fields(person_id = self.delegate.id().external()))]
    fn replace_next_trip(&mut self, response: InternalRoutingResponse, _now: SimTime) {
        trace!(uuid = response.request_id.as_u128());

        if response.elements.is_empty() {
            // If the response is empty, we do not replace anything.
            return;
        }

        let plan = self.delegate.basic_agent_delegate.selected_plan_mut();
        let start_index = self.delegate.curr_plan_element;

        let span = find_trip_span_starting_at_activity_default(&plan.elements, start_index)
            .expect("No trip found starting at the current plan element");

        // Replace the trip elements (legs and intermediate activities) with the new response
        span.replace_trip_elements(&mut plan.elements, response.elements);
    }
}

/// Returns the link and coordinate under which `activity` is sent to the routing service for a trip
/// with routing `mode`.
///
/// Activities at a facility are connected to the network via the facility's modal link for `mode`,
/// exactly as in the in-process routing (see [`Facility::modal_link`], which falls back to the base
/// link). Activities without a facility use their own link.
fn routing_location(
    scenario_core: &ScenarioCore,
    activity: &InternalActivity,
    mode: &str,
) -> (String, Coordinate) {
    let Some(facility_id) = &activity.facility_id else {
        return (
            activity.link_id().external().to_string(),
            activity.coord().clone(),
        );
    };

    let facility = scenario_core
        .facilities
        .get(facility_id)
        .unwrap_or_else(|| {
            panic!(
                "Activity of type {} references unknown facility {}. Facilities are checked in prepare_for_sim.",
                activity.act_type.external(),
                facility_id.external()
            )
        });
    let facility = Facility::ActivityFacility(facility);
    (
        facility
            .modal_link(&Id::get_from_ext(mode))
            .external()
            .to_string(),
        facility.coord().clone(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::external_services::routing::InternalRoutingResponse;
    use crate::simulation::InternalAttributes;
    use crate::simulation::id::Id;
    use crate::simulation::scenario::Coordinate;
    use crate::simulation::scenario::facilities::{ActivityFacilities, ActivityFacility};
    use crate::simulation::scenario::population::{
        InternalActivity, InternalLeg, InternalPlan, InternalRoute,
    };
    use macros::deterministic_id_test;
    use nohash_hasher::IntMap;
    use std::sync::Arc;
    use uuid::Uuid;

    /// A core with the facility "f1" at (1, 2) whose base link is "base" and whose modal links are
    /// given as `(mode, link)` pairs.
    fn scenario_core_with_facility(modal_links: &[(&str, &str)]) -> ScenarioCore {
        let mode_to_link: IntMap<_, _> = modal_links
            .iter()
            .map(|(mode, link)| (Id::create(mode), Id::create(link)))
            .collect();
        let mut facilities = ActivityFacilities::default();
        facilities.add_facility(ActivityFacility {
            id: Id::create("f1"),
            coord: Coordinate::new_2d(1.0, 2.0),
            base_link: Some(Id::create("base")),
            mode_to_link,
            desc: None,
            activities: Vec::new(),
            attributes: InternalAttributes::default(),
        });
        ScenarioCore {
            facilities: Arc::new(facilities),
            ..Default::default()
        }
    }

    fn make_facility_activity(facility: &str, link: &str) -> InternalActivity {
        InternalActivity {
            facility_id: Some(Id::create(facility)),
            ..make_activity("work", link)
        }
    }

    #[deterministic_id_test]
    fn routing_location_uses_modal_link_of_facility() {
        let core = scenario_core_with_facility(&[("car", "car-link")]);
        // After prepare_for_sim, the link of an activity at a facility is the facility's base link.
        let activity = make_facility_activity("f1", "base");

        let (link, coord) = routing_location(&core, &activity, "car");

        assert_eq!("car-link", link);
        assert_eq!(Coordinate::new_2d(1.0, 2.0), coord);
    }

    #[deterministic_id_test]
    fn routing_location_falls_back_to_base_link_without_modal_link() {
        let core = scenario_core_with_facility(&[("car", "car-link")]);
        Id::<String>::create("bike");
        let activity = make_facility_activity("f1", "base");

        let (link, coord) = routing_location(&core, &activity, "bike");

        assert_eq!("base", link);
        assert_eq!(Coordinate::new_2d(1.0, 2.0), coord);
    }

    #[deterministic_id_test]
    fn routing_location_uses_activity_link_without_facility() {
        let core = scenario_core_with_facility(&[("car", "car-link")]);
        let activity = make_activity("home", "l1");

        let (link, coord) = routing_location(&core, &activity, "car");

        assert_eq!("l1", link);
        assert_eq!(Coordinate::default(), coord);
    }

    #[deterministic_id_test]
    #[should_panic(expected = "references unknown facility missing")]
    fn routing_location_panics_for_unknown_facility() {
        let core = scenario_core_with_facility(&[("car", "car-link")]);
        let activity = make_facility_activity("missing", "base");

        routing_location(&core, &activity, "car");
    }

    fn make_activity(act_type: &str, link: &str) -> InternalActivity {
        InternalActivity {
            act_type: Id::create(act_type),
            link_id: Some(Id::create(link)),
            coord: Some(Coordinate::default()),
            facility_id: None,
            start_time: None,
            end_time: None,
            max_dur: None,
            attributes: Default::default(),
        }
    }

    fn make_leg(mode: &str) -> InternalLeg {
        InternalLeg {
            mode: Id::create(mode),
            routing_mode: Some(Id::create(mode)),
            dep_time: None,
            trav_time: Some(Duration::from_secs(10)),
            route: Some(InternalRoute::Generic(
                crate::simulation::scenario::population::InternalGenericRoute::new(
                    Id::create("l1"),
                    Id::create("l2"),
                    Some(Duration::from_secs(10)),
                    Some(100.0),
                    None,
                ),
            )),
            attributes: Default::default(),
        }
    }

    fn person_with_leg(route: InternalRoute) -> InternalPerson {
        let mut plan = InternalPlan::default();
        let mut first = make_activity("home", "l1");
        first.max_dur = Some(Duration::from_secs(10));
        plan.add_act(first);
        let mut leg = make_leg("car");
        leg.route = Some(route);
        plan.add_leg(leg);
        let mut last = make_activity("work", "l2");
        last.max_dur = Some(Duration::from_secs(7));
        plan.add_act(last);
        InternalPerson::new(Id::create("person"), plan)
    }

    fn generic_route() -> crate::simulation::scenario::population::InternalGenericRoute {
        crate::simulation::scenario::population::InternalGenericRoute::new(
            Id::create("l1"),
            Id::create("l2"),
            Some(Duration::from_secs(10)),
            Some(100.0),
            None,
        )
    }

    #[deterministic_id_test]
    fn phases_preserve_times_and_materialize_the_complete_person() {
        use crate::simulation::agents::agent::SimulationAgent;
        for adaptive in [false, true] {
            let person = person_with_leg(InternalRoute::Generic(generic_route()));
            let expected = person.clone();
            let mut agent = if adaptive {
                SimulationAgent::new_adaptive_plan_based(person)
            } else {
                SimulationAgent::new_plan_based(person)
            };
            let activity = agent.activity();
            assert_eq!(activity.act_type().external(), "home");
            assert_eq!(activity.link_id().external(), "l1");
            assert_eq!(activity.coord(), &Coordinate::default());
            assert!(activity.has_next_leg());
            assert_eq!(
                activity.end_time(SimTime::default()),
                SimTime::from_secs(10)
            );
            assert_eq!(
                activity.wakeup_time(SimTime::default()),
                SimTime::from_secs(10)
            );
            agent.activity_mut().on_finished(SimTime::from_secs(10));
            // Completion callbacks do not switch phase before the receiving engine runs.
            assert!(matches!(agent.phase(), AgentPhase::Activity(_)));
            agent.activity_mut().start_next_leg(SimTime::from_secs(10));
            let leg = agent.leg();
            assert_eq!(leg.mode().external(), "car");
            assert_eq!(leg.routing_mode().map(Id::external), Some("car"));
            assert_eq!(leg.departure_link().external(), "l1");
            assert!(leg.network_driver().is_none());
            assert_eq!(leg.curr_link_id(), Some(&Id::get_from_ext("l1")));
            assert_eq!(
                agent.teleportation().arrival_time(SimTime::from_secs(10)),
                SimTime::from_secs(20)
            );
            assert_eq!(agent.teleportation().distance(), Some(100.0));
            assert!(agent.teleportation().transit().is_none());
            agent
                .teleportation_mut()
                .begin_teleportation(SimTime::from_secs(10));
            assert_eq!(agent.leg().curr_link_id(), Some(&Id::get_from_ext("l2")));
            assert!(matches!(agent.phase(), AgentPhase::Leg(_)));
            agent.leg_mut().start_next_activity(SimTime::from_secs(21));
            assert_eq!(
                agent.activity().end_time(SimTime::from_secs(100)),
                SimTime::from_secs(28)
            );
            assert_eq!(agent.activity().act_type().external(), "work");
            assert!(!agent.activity().has_next_leg());
            assert_eq!(agent.into_person(), expected);
        }
    }

    #[deterministic_id_test]
    fn network_roles_follow_route_positions_including_round_trips_and_single_link_routes() {
        use crate::simulation::agents::agent::SimulationAgent;
        use crate::simulation::scenario::population::InternalNetworkRoute;
        for adaptive in [false, true] {
            for links in [vec!["l1"], vec!["l1", "l2"], vec!["l1", "l2", "l1"]] {
                let generic = crate::simulation::scenario::population::InternalGenericRoute::new(
                    Id::create("l1"),
                    Id::create(links.last().unwrap()),
                    Some(Duration::from_secs(10)),
                    Some(100.0),
                    Some(Id::create("vehicle")),
                );
                let route = InternalRoute::Network(InternalNetworkRoute::new(
                    generic,
                    links.iter().map(|id| Id::get_from_ext(id)).collect(),
                ));
                let person = person_with_leg(route);
                let mut agent = if adaptive {
                    SimulationAgent::new_adaptive_plan_based(person)
                } else {
                    SimulationAgent::new_plan_based(person)
                };
                agent.activity_mut().start_next_leg(SimTime::default());
                assert!(agent.leg().teleportation().is_some());
                assert_eq!(
                    agent
                        .network_driver()
                        .planned_vehicle_id()
                        .map(Id::external),
                    Some("vehicle")
                );
                for (index, link) in links.iter().enumerate() {
                    assert_eq!(agent.leg().curr_link_id().unwrap().external(), *link);
                    let driver = agent.network_driver();
                    assert_eq!(
                        driver.is_wanting_to_arrive_on_current_link(),
                        index + 1 == links.len()
                    );
                    assert_eq!(
                        driver.peek_next_link_id().map(Id::external),
                        links.get(index + 1).copied()
                    );
                    if let Some(next) = driver.peek_next_link_id().cloned() {
                        agent
                            .network_driver_mut()
                            .move_over_node(&next, SimTime::default());
                    }
                }
                agent
                    .teleportation_mut()
                    .begin_teleportation(SimTime::default());
                assert_eq!(
                    agent.leg().curr_link_id().unwrap().external(),
                    *links.last().unwrap()
                );
                assert_eq!(
                    agent.teleportation().destination_link().external(),
                    *links.last().unwrap()
                );
            }
        }
    }

    #[deterministic_id_test]
    fn read_and_write_roles_are_offered_together() {
        use crate::simulation::agents::agent::SimulationAgent;
        use crate::simulation::scenario::population::{
            InternalNetworkRoute, InternalPtRoute, InternalPtRouteDescription,
        };
        let pt = InternalRoute::Pt(InternalPtRoute {
            generic_delegate: generic_route(),
            description: InternalPtRouteDescription {
                transit_route_id: "route".into(),
                boarding_time: None,
                transit_line_id: "line".into(),
                access_facility_id: "from".into(),
                egress_facility_id: "to".into(),
            },
        });
        let network = InternalRoute::Network(InternalNetworkRoute::new(
            generic_route(),
            vec![Id::get_from_ext("l1"), Id::get_from_ext("l2")],
        ));
        for (route, drives) in [
            (InternalRoute::Generic(generic_route()), false),
            (network, true),
            (pt, false),
        ] {
            for adaptive in [false, true] {
                let person = person_with_leg(route.clone());
                let mut agent = if adaptive {
                    SimulationAgent::new_adaptive_plan_based(person)
                } else {
                    SimulationAgent::new_plan_based(person)
                };
                agent.activity_mut().start_next_leg(SimTime::default());
                assert_eq!(agent.leg().network_driver().is_some(), drives);
                assert!(agent.leg().teleportation().is_some());
                assert_eq!(agent.leg_mut().into_network_driver().is_some(), drives);
                assert!(agent.leg_mut().into_teleportation().is_some());
            }
        }
    }

    #[deterministic_id_test]
    fn pt_routes_only_offer_teleportation_and_keep_their_metadata() {
        use crate::simulation::agents::agent::SimulationAgent;
        use crate::simulation::scenario::population::{
            InternalPtRoute, InternalPtRouteDescription,
        };
        let route = InternalRoute::Pt(InternalPtRoute {
            generic_delegate: generic_route(),
            description: InternalPtRouteDescription {
                transit_route_id: "route".into(),
                boarding_time: Some(SimTime::from_secs(12)),
                transit_line_id: "line".into(),
                access_facility_id: "from".into(),
                egress_facility_id: "to".into(),
            },
        });
        let mut agent = SimulationAgent::new_plan_based(person_with_leg(route.clone()));
        agent.activity_mut().start_next_leg(SimTime::default());
        assert!(agent.leg().network_driver().is_none());
        assert_eq!(
            agent.teleportation().transit(),
            Some(TransitTeleportation {
                line: "line",
                route: "route",
                boarding_time: Some(SimTime::from_secs(12)),
                access_facility: "from",
                egress_facility: "to",
            })
        );
        assert_eq!(agent.teleportation().distance(), Some(100.0));
        assert_eq!(agent.teleportation().destination_link(), route.end_link());
        agent
            .teleportation_mut()
            .begin_teleportation(SimTime::default());
        assert_eq!(agent.leg().curr_link_id(), Some(route.end_link()));
    }

    #[deterministic_id_test]
    fn ready_route_is_installed_when_the_activity_finishes_not_at_wakeup() {
        let mut plan = InternalPlan::default();
        plan.add_act(make_activity("home", "1"));
        plan.add_leg(make_leg("car"));
        plan.add_act(make_activity("work", "2"));
        let mut logic =
            AdaptivePlanBasedSimulationLogic::new(InternalPerson::new(Id::create("p1"), plan));
        let (send, recv) = tokio::sync::oneshot::channel();
        assert!(
            send.send(InternalRoutingResponse {
                elements: vec![InternalPlanElement::Leg(make_leg("bike"))],
                request_id: Uuid::now_v7(),
            })
            .is_ok()
        );
        logic.inner.route_receiver = Some(recv);
        let next_mode = |logic: &AdaptivePlanBasedSimulationLogic| {
            logic
                .inner
                .delegate
                .next_leg()
                .unwrap()
                .mode
                .external()
                .to_owned()
        };

        let mut env = ThreadLocalComputationalEnvironment::default();
        logic.phase_mut().expect_activity().on_wakeup(
            &mut env,
            SimTime::from_secs(5),
            SimTime::from_secs(5),
        );
        assert_eq!(next_mode(&logic), "car");
        logic
            .phase_mut()
            .expect_activity()
            .on_finished(SimTime::from_secs(10));
        assert_eq!(next_mode(&logic), "bike");
    }

    #[deterministic_id_test]
    fn materialization_preserves_unfinished_routes_and_all_plans() {
        use crate::simulation::agents::agent::SimulationAgent;
        use crate::simulation::scenario::population::InternalNetworkRoute;
        for adaptive in [false, true] {
            let mut plan = InternalPlan::default();
            plan.add_act(make_activity("home", "a"));
            let mut leg = make_leg("car");
            leg.route = Some(InternalRoute::Network(InternalNetworkRoute::new(
                generic_route(),
                ["a", "b", "c"].into_iter().map(Id::create).collect(),
            )));
            plan.add_leg(leg);
            plan.add_act(make_activity("work", "c"));
            let mut person = InternalPerson::new(Id::create("person"), plan);
            person.plans_mut()[0].score = Some(42.0);
            let mut unselected = person.plans()[0].clone();
            unselected.selected = false;
            unselected.score = Some(-1.0);
            unselected.elements[1].as_leg_mut().unwrap().route = None;
            // The selected plan is not the first one, so the agent must not assume index 0.
            person.plans_mut().insert(0, unselected);
            let expected = person.clone();
            let mut agent = if adaptive {
                SimulationAgent::new_adaptive_plan_based(person)
            } else {
                SimulationAgent::new_plan_based(person)
            };
            agent.activity_mut().start_next_leg(SimTime::from_secs(5));
            agent
                .network_driver_mut()
                .move_over_node(&Id::get_from_ext("b"), SimTime::from_secs(6));
            assert_eq!(agent.leg().curr_link_id().unwrap().external(), "b");
            assert_eq!(agent.into_person(), expected);
        }
    }

    #[deterministic_id_test]
    #[should_panic(expected = "Agent person: expected Leg phase, found Activity")]
    fn wrong_phase_fails_at_the_capability_boundary() {
        use crate::simulation::agents::agent::SimulationAgent;
        let agent = SimulationAgent::new_plan_based(person_with_leg(InternalRoute::Generic(
            generic_route(),
        )));
        agent.leg();
    }

    #[deterministic_id_test]
    #[should_panic(expected = "Agent person: expected NetworkDriver role")]
    fn missing_role_fails_at_the_capability_boundary() {
        use crate::simulation::agents::agent::SimulationAgent;
        let mut agent = SimulationAgent::new_plan_based(person_with_leg(InternalRoute::Generic(
            generic_route(),
        )));
        agent.activity_mut().start_next_leg(SimTime::default());
        agent.network_driver_mut();
    }

    #[deterministic_id_test]
    fn test_replace_next_trip_basic() {
        // Plan: home --leg1--> work --leg2--> shop
        let mut plan = InternalPlan::default();
        plan.add_act(make_activity("home", "1"));
        plan.add_leg(make_leg("car"));
        plan.add_act(make_activity("work", "2"));
        plan.add_leg(make_leg("walk"));
        plan.add_act(make_activity("home", "3"));
        let person = InternalPerson::new(Id::create("p1"), plan);
        let mut logic = AdaptivePlanBasedSimulationLogic::new(person);

        // Replace the first trip (home->work)
        let response = InternalRoutingResponse {
            elements: vec![InternalPlanElement::Leg(make_leg("bike"))],
            request_id: Uuid::now_v7(),
        };

        logic
            .inner
            .replace_next_trip(response.clone(), SimTime::default());
        let elements = &logic
            .inner
            .delegate
            .basic_agent_delegate
            .selected_plan()
            .unwrap()
            .elements;

        assert_eq!(
            elements[0].as_activity().unwrap().act_type.external(),
            "home"
        );
        assert_eq!(elements[1].as_leg().unwrap().mode.external(), "bike");
        assert_eq!(
            elements[2].as_activity().unwrap().act_type.external(),
            "work"
        );
        assert_eq!(elements[3].as_leg().unwrap().mode.external(), "walk");
        assert_eq!(
            elements[4].as_activity().unwrap().act_type.external(),
            "home"
        );
    }
}
