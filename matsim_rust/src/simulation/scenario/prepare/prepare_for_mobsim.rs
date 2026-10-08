use super::{PlanPreparationFailure, PrepareError, owned_plan, prepare_person};
use crate::simulation::config::Config;
use crate::simulation::id::Id;
use crate::simulation::replanning::routing::utils::calc_distance;
use crate::simulation::replanning::routing::{
    Facility, RoutingError, RoutingRequestBuilder, TripRouter,
};
use crate::simulation::scenario::ControllerScenario;
use crate::simulation::scenario::facilities::ActivityFacilities;
use crate::simulation::scenario::network::{Link, Network};
use crate::simulation::scenario::population::{
    InternalActivity, InternalGenericRoute, InternalLeg, InternalPerson, InternalPlan,
    InternalPlanElement, InternalRoute,
};
use crate::simulation::scenario::trip_structure_utils::{
    TripSpan, get_trip_spans_default, identify_main_mode,
};
use crate::simulation::scenario::vehicles::{Garage, InternalVehicle};
use crate::simulation::time::SimTime;
use crate::simulation::time::time_interpretation::TimeInterpretation;
use rayon::prelude::*;
use std::borrow::Cow;
use thiserror::Error;

/// Prepares the population before every mobsim iteration: plans are validated and repaired, e.g.
/// trips without valid routes are routed with the current travel times.
pub(crate) fn prepare_for_mobsim(
    scenario: &mut ControllerScenario,
    trip_router: &TripRouter,
) -> Result<(), PrepareError> {
    let context = PrepareForMobsimContext {
        network: &scenario.core.network,
        garage: &scenario.core.garage,
        facilities: &scenario.core.facilities,
        config: scenario.core.config.as_ref(),
    };

    let issues: Vec<_> = scenario
        .population
        .persons
        .par_iter_mut()
        .flat_map(|(_, person)| {
            prepare_person(person, |person, plan| {
                prepare_plan(&context, person, plan, trip_router)
            })
        })
        .collect();

    PrepareError::from_issues(issues)
}

pub struct PrepareForMobsimContext<'a> {
    pub network: &'a Network,
    pub garage: &'a Garage,
    pub facilities: &'a ActivityFacilities,
    pub config: &'a Config,
}

#[derive(Debug, Error)]
pub(crate) enum TripPreparationError {
    #[error("Trip contains no legs")]
    NoLegs,
    #[error("Trip has no unambiguous routing mode")]
    AmbiguousMainMode,
    #[error("Could not derive the trip departure time from the plan")]
    MissingDepartureTime,
    #[error(
        "No vehicle found for network mode {mode} (expected default vehicle {default_vehicle})"
    )]
    MissingVehicle {
        mode: String,
        default_vehicle: String,
    },
    #[error("Activity references unknown facility {facility}")]
    UnknownFacility { facility: String },
    #[error(transparent)]
    Routing(#[from] RoutingError),
}

enum TripAssessment {
    Valid,
    NeedsRouting(Id<String>),
}

fn prepare_plan(
    context: &PrepareForMobsimContext<'_>,
    person: &InternalPerson,
    plan: &InternalPlan,
    trip_router: &TripRouter,
) -> Result<Option<InternalPlan>, PlanPreparationFailure> {
    // `Cow` works as follows: borrow the plan and if it needs to be mutated, clone it.
    let mut working_plan = Cow::Borrowed(plan);

    let trip_count = get_trip_spans_default(&working_plan.elements).len();
    for trip_index in 0..trip_count {
        check_and_adapt_trip(context, person, &mut working_plan, trip_index, trip_router).map_err(
            |source| PlanPreparationFailure {
                trip_index: Some(trip_index),
                message: source.to_string(),
            },
        )?;
    }

    Ok(owned_plan(working_plan))
}

fn check_and_adapt_trip(
    context: &PrepareForMobsimContext<'_>,
    person: &InternalPerson,
    working_plan: &mut Cow<'_, InternalPlan>,
    trip_index: usize,
    trip_router: &TripRouter,
) -> Result<(), TripPreparationError> {
    let span = get_trip_spans_default(&working_plan.elements)
        .get(trip_index)
        .copied()
        .expect("routing modules must preserve the number of trips");
    let TripAssessment::NeedsRouting(mode) = assess_trip(context, span, working_plan)? else {
        return Ok(());
    };

    let new_elements = route_trip(context, person, working_plan, span, &mode, trip_router)?;

    span.replace_trip_elements(&mut working_plan.to_mut().elements, new_elements);
    Ok(())
}

pub(crate) fn route_trip(
    context: &PrepareForMobsimContext<'_>,
    person: &InternalPerson,
    plan: &InternalPlan,
    span: TripSpan,
    mode: &Id<String>,
    trip_router: &TripRouter,
) -> Result<Vec<InternalPlanElement>, TripPreparationError> {
    let departure_time = TimeInterpretation::decide_on_elements_end_time(
        &plan.elements[..=span.origin_index()],
        &SimTime::default(),
    )
    .ok_or(TripPreparationError::MissingDepartureTime)?;

    let origin = span.origin(&plan.elements);
    let dest = span.destination(&plan.elements);
    let from_facility = facility_for_activity(context, origin, mode)?;
    let to_facility = facility_for_activity(context, dest, mode)?;
    let vehicle = vehicle_for_trip(context, person, span, &plan.elements, mode)?;
    let request = RoutingRequestBuilder::default()
        .from(&from_facility)
        .to(&to_facility)
        .departure_time(departure_time)
        .person(Some(person))
        .vehicle(vehicle)
        .build()
        .expect("all required routing request fields are set");
    Ok(trip_router.calc_route(mode, request)?)
}

/// Each activity takes place at a facility. Activities without a facility are routed via a link
/// wrapper facility built from the activity's own link and coordinate. Its modal link for the
/// routing `mode` is computed on the fly, see [`Facility::new_link_wrapper_for_mode`].
fn facility_for_activity<'a>(
    context: &PrepareForMobsimContext<'a>,
    activity: &InternalActivity,
    mode: &Id<String>,
) -> Result<Facility<'a>, TripPreparationError> {
    match &activity.facility_id {
        Some(facility_id) => context
            .facilities
            .get(facility_id)
            .map(Facility::ActivityFacility)
            .ok_or_else(|| TripPreparationError::UnknownFacility {
                facility: facility_id.external().to_string(),
            }),
        None => Ok(Facility::new_link_wrapper_for_mode(
            activity.coord().clone(),
            activity.link_id().clone(),
            mode,
            context.network,
            context.config.facilities().modal_link_selection,
        )),
    }
}

fn assess_trip(
    context: &PrepareForMobsimContext<'_>,
    span: TripSpan,
    working_plan: &mut Cow<'_, InternalPlan>,
) -> Result<TripAssessment, TripPreparationError> {
    let trip_elements = span.trip_elements(&working_plan.elements);
    if !trip_elements
        .iter()
        .any(|element| element.as_leg().is_some())
    {
        return Err(TripPreparationError::NoLegs);
    }
    let mode = identify_main_mode(trip_elements)
        .map(|mode| Id::get_from_ext(&mode))
        .ok_or(TripPreparationError::AmbiguousMainMode)?;

    add_travel_distance(context.network, span, working_plan);
    synchronize_missing_travel_times(span, working_plan);

    let elements = &working_plan.elements;
    let legs: Vec<_> = span.legs(elements).collect();

    if trip_is_valid(context, span, elements, &mode, &legs) {
        Ok(TripAssessment::Valid)
    } else {
        Ok(TripAssessment::NeedsRouting(mode))
    }
}

fn add_travel_distance(
    network: &Network,
    span: TripSpan,
    working_plan: &mut Cow<'_, InternalPlan>,
) {
    // check: if there is a leg that has a network route but no distance, then we need to calculate it.
    let needs_dist_calc = span
        .legs(&working_plan.elements)
        .filter_map(|l| l.route.as_ref())
        .filter_map(|r| r.as_network())
        .any(|n| n.generic_delegate().distance().is_none());
    if !needs_dist_calc {
        return;
    }

    for leg in span.legs_mut(&mut working_plan.to_mut().elements) {
        let Some(route) = leg.route.as_mut() else {
            continue;
        };
        let distance = match route {
            InternalRoute::Network(network_route) => {
                calc_distance(network_route, 1.0, 1.0, network)
            }
            _ => continue,
        };
        route.as_generic_mut().set_distance(Some(distance));
    }
}

/// Copies a travel time from a leg to its route or vice versa when exactly one is set.
/// The plan is cloned only if synchronization is actually necessary.
fn synchronize_missing_travel_times(span: TripSpan, working_plan: &mut Cow<'_, InternalPlan>) {
    let needs_synchronization = span.legs(&working_plan.elements).any(|leg| {
        leg.route.as_ref().is_some_and(|route| {
            leg.trav_time.is_some() != route.as_generic().trav_time().is_some()
        })
    });

    if !needs_synchronization {
        return;
    }

    for leg in span.legs_mut(&mut working_plan.to_mut().elements) {
        let leg_travel_time = leg.trav_time;
        let Some(route) = leg.route.as_mut() else {
            continue;
        };
        let route_travel_time = route.as_generic().trav_time();

        match (leg_travel_time, route_travel_time) {
            (None, Some(travel_time)) => leg.trav_time = Some(travel_time),
            (Some(travel_time), None) => {
                route.as_generic_mut().set_trav_time(Some(travel_time));
            }
            _ => {}
        }
    }
}

fn trip_is_valid(
    context: &PrepareForMobsimContext<'_>,
    span: TripSpan,
    elements: &[InternalPlanElement],
    mode: &Id<String>,
    legs: &[&InternalLeg],
) -> bool {
    let any_leg_wrong_routing_mode = legs
        .iter()
        .any(|leg| leg.routing_mode.as_ref() != Some(mode));
    if any_leg_wrong_routing_mode {
        return false;
    }

    for leg in legs {
        let Some(route) = leg.route.as_ref() else {
            return false;
        };
        let generic = route.as_generic();

        if !generic_route_is_valid(generic) {
            return false;
        }

        // QSim derives travel times for main-mode legs. All other legs need a travel time,
        // which is present on both leg and route after synchronization above.
        if !is_network_mode(context, &leg.mode)
            && (leg.trav_time.is_none() || generic.trav_time().is_none())
        {
            return false;
        }

        if let InternalRoute::Network(network_route) = route
            && !network_route_is_valid(context.network, network_route.route(), &leg.mode, generic)
        {
            return false;
        }
    }

    if is_network_mode(context, mode) {
        let access_egress_mode = &context.config.routing().access_egress_mode;
        let first_is_access_egress = legs
            .first()
            .is_some_and(|leg| leg.mode.external() == access_egress_mode);
        let last_is_access_egress = legs
            .last()
            .is_some_and(|leg| leg.mode.external() == access_egress_mode);
        let has_network_main_leg = legs
            .iter()
            .any(|leg| &leg.mode == mode && matches!(leg.route, Some(InternalRoute::Network(_))));
        let interaction_count = span
            .trip_elements(elements)
            .iter()
            .filter_map(InternalPlanElement::as_activity)
            .filter(|activity| activity.is_interaction())
            .count();
        if !first_is_access_egress
            || !last_is_access_egress
            || !has_network_main_leg
            || interaction_count < 2
        {
            return false;
        }
    }

    true
}

/// Checks whether distance is set.
fn generic_route_is_valid(route: &InternalGenericRoute) -> bool {
    let Some(distance) = route.distance() else {
        return false;
    };
    distance.is_finite() && distance >= 0.0
}

/// Checks if a given network route is valid. This is the case if the route starts and ends with the correct links,
/// all links in the route support the given mode, and all links are connected in sequence.
fn network_route_is_valid(
    network: &Network,
    route: &[Id<Link>],
    mode: &Id<String>,
    generic: &InternalGenericRoute,
) -> bool {
    if route.first() != Some(generic.start_link()) || route.last() != Some(generic.end_link()) {
        return false;
    }

    let links = route
        .iter()
        .map(|link_id| network.get_link(link_id))
        .collect::<Vec<_>>();
    links.iter().all(|link| link.contains_mode(mode))
        && links.windows(2).all(|pair| pair[0].to == pair[1].from)
}

fn vehicle_for_trip<'a>(
    context: &'a PrepareForMobsimContext<'_>,
    person: &InternalPerson,
    span: TripSpan,
    elements: &[InternalPlanElement],
    mode: &Id<String>,
) -> Result<Option<&'a InternalVehicle>, TripPreparationError> {
    if !is_network_mode(context, mode) {
        return Ok(None);
    }

    if let Some(vehicle) = span
        .legs(elements)
        .filter(|leg| &leg.mode == mode)
        .filter_map(|leg| leg.route.as_ref())
        .filter(|route| matches!(route, InternalRoute::Network(_)))
        .filter_map(|route| route.as_generic().vehicle().as_ref())
        .find_map(|vehicle_id| context.garage.vehicles.get(vehicle_id))
    {
        return Ok(Some(vehicle));
    }

    let default_id = format!("{}_{}", person.id().external(), mode.external());
    context
        .garage
        .vehicles
        .iter()
        .find(|(id, _)| id.external() == default_id)
        .map(|(_, vehicle)| Some(vehicle))
        .ok_or_else(|| TripPreparationError::MissingVehicle {
            mode: mode.external().to_string(),
            default_vehicle: default_id,
        })
}

fn is_network_mode(context: &PrepareForMobsimContext<'_>, mode: &Id<String>) -> bool {
    context
        .config
        .qsim()
        .main_modes
        .iter()
        .any(|candidate| candidate == mode.external())
}

#[cfg(test)]
mod tests {
    use super::add_travel_distance;
    use super::prepare_for_mobsim;
    use crate::simulation::InternalAttributes;
    use crate::simulation::config::{
        Config, ModalLinkSelection, TransitRangeQuerySettings, TransitRouteSelectorSettings,
    };
    use crate::simulation::id::Id;
    use crate::simulation::network::signals::Signals;
    use crate::simulation::replanning::routing::teleportation::TeleportationRoutingModule;
    use crate::simulation::replanning::routing::{
        RoutingError, RoutingModule, RoutingRequest, TransitRoutingModule, TripRouter,
    };
    use crate::simulation::scenario::facilities::ActivityFacilities;
    use crate::simulation::scenario::network::{Link, Network, Node};
    use crate::simulation::scenario::population::{
        InternalActivity, InternalGenericRoute, InternalLeg, InternalNetworkRoute, InternalPerson,
        InternalPlan, InternalPlanElement, InternalRoute, Population,
    };
    use crate::simulation::scenario::prepare::prepare_for_sim::prepare_for_sim;
    use crate::simulation::scenario::prepare::test_utils::{
        activity_facility, facilities, layered_network, located_activity,
    };
    use crate::simulation::scenario::transit::TransitSchedule;
    use crate::simulation::scenario::trip_structure_utils::get_trip_spans_default;
    use crate::simulation::scenario::vehicles::{Garage, InternalVehicle, InternalVehicleType};
    use crate::simulation::scenario::{ControllerScenario, Coordinate, Scenario};
    use crate::simulation::time::SimTime;
    use macros::deterministic_id_test;
    use nohash_hasher::{IntMap, IntSet};
    use std::borrow::Cow;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    // Before: no persons or plans; after: the population is still empty.
    #[deterministic_id_test]
    fn prepare_for_mobsim_succeeds_for_empty_population() {
        let mut scenario = scenario_with_population(Population::new());

        prepare_for_mobsim(&mut scenario, &empty_router()).unwrap();

        assert!(scenario.population.persons.is_empty());
    }

    // Before: two persons with activity-only plans; after: both persons and plans are unchanged.
    #[deterministic_id_test]
    fn prepare_for_mobsim_visits_population_without_moving_persons() {
        let mut persons = IntMap::default();
        persons.insert(Id::create("person-1"), person("person-1", "link-1"));
        persons.insert(Id::create("person-2"), person("person-2", "link-1"));
        let mut scenario = scenario_with_network_and_population(
            network_with_link(Id::create("link-1")),
            Population { persons },
        );

        prepare_for_mobsim(&mut scenario, &empty_router()).unwrap();

        assert_eq!(2, scenario.population.persons.len());
        assert!(
            scenario
                .population
                .persons
                .contains_key(&Id::get_from_ext("person-1"))
        );
        assert!(
            scenario
                .population
                .persons
                .contains_key(&Id::get_from_ext("person-2"))
        );
    }

    #[deterministic_id_test]
    fn prepares_a_transit_range_route_into_the_selected_plan() {
        let person_id = Id::create("range-person");
        let mut plan = InternalPlan::default();
        plan.add_act(InternalActivity::new(
            Some(Coordinate::new_2d(1050.0, 1050.0)),
            "home",
            Id::create("link-1"),
            None,
            Some(SimTime::from_secs(8 * 3600)),
            None,
        ));
        plan.add_leg(unrouted_leg("pt"));
        plan.add_act(InternalActivity::new(
            Some(Coordinate::new_2d(3950.0, 1050.0)),
            "work",
            Id::create("link-2"),
            None,
            None,
            None,
        ));
        let mut persons = IntMap::default();
        persons.insert(
            person_id.clone(),
            InternalPerson::new(person_id.clone(), plan),
        );
        let mut modules: IntMap<Id<String>, Arc<dyn RoutingModule>> = IntMap::default();
        let mode = Id::create("pt");
        modules.insert(
            mode.clone(),
            Arc::new(
                TransitRoutingModule::new(
                    Arc::new(TransitSchedule::from_file(
                        "./tests/resources/pt_reference/routing_direct_vs_transfer/transit_schedule.xml"
                            .as_ref(),
                    )),
                    0.8333333333333334,
                    1.3,
                    Arc::new(Garage::default()),
                    None,
                )
                .with_range_queries(
                    vec![TransitRangeQuerySettings {
                        max_earlier_departure_sec: 120,
                        max_later_departure_sec: 120,
                        subpopulations: Vec::new(),
                    }],
                    vec![TransitRouteSelectorSettings::default()],
                    4711,
                ),
            ),
        );
        let mut scenario = scenario_with_parts(
            sequential_network(2, None),
            Garage::default(),
            Population { persons },
            Config::default(),
        );

        prepare_for_mobsim(&mut scenario, &TripRouter::new(modules)).unwrap();

        let prepared = scenario.population.persons[&person_id]
            .selected_plan()
            .unwrap();
        assert!(
            prepared.legs().len() >= 3,
            "access, transit and egress legs remain in the plan"
        );
        assert!(
            prepared
                .legs()
                .iter()
                .any(|leg| leg.mode.external() == "pt")
        );
        assert!(
            prepared
                .legs()
                .iter()
                .all(|leg| leg.routing_mode.as_ref() == Some(&mode))
        );
        assert_eq!(
            Some(SimTime::from_secs(8 * 3600)),
            prepared.acts()[0].end_time
        );
    }

    // Before: two act--unrouted walk--act plans; after: both contain valid walk legs and remain stable.
    #[deterministic_id_test]
    fn repairs_all_teleported_plans_and_keeps_valid_shape() {
        let network = sequential_network(2, None);
        let mut first_plan = unrouted_plan("walk", "link-1", "link-2", 10);
        let second_plan = unrouted_plan("walk", "link-1", "link-2", 20);
        let person_id = Id::create("person-1");
        let mut person = InternalPerson::new(person_id.clone(), first_plan.clone());
        person.plans_mut().push(second_plan);
        let mut persons = IntMap::default();
        persons.insert(person_id.clone(), person);

        let router = teleportation_router("walk");
        let mut scenario = scenario_with_parts(
            network,
            Garage::default(),
            Population { persons },
            Config::default(),
        );

        prepare_for_mobsim(&mut scenario, &router).unwrap();

        let person = scenario.population.persons.get(&person_id).unwrap();
        assert_eq!(2, person.plans().len());
        for (index, plan) in person.plans().iter().enumerate() {
            let legs = plan.legs();
            assert_eq!(1, legs.len());
            assert!(matches!(legs[0].route, Some(InternalRoute::Generic(_))));
            assert_eq!(
                Some(SimTime::from_secs(if index == 0 { 10 } else { 20 })),
                legs[0].dep_time
            );
            assert_eq!(Some(Id::get_from_ext("walk")), legs[0].routing_mode);
        }

        first_plan = person.plans()[0].clone();
        prepare_for_mobsim(&mut scenario, &empty_router()).unwrap();
        assert_eq!(
            &first_plan,
            &scenario.population.persons.get(&person_id).unwrap().plans()[0]
        );
    }

    #[deterministic_id_test]
    fn copies_travel_time_from_route_to_leg() {
        let travel_time = Duration::from_secs(7);
        let plan = routed_plan("walk", None, Some(travel_time));
        let person_id = Id::create("person-1");
        let mut persons = IntMap::default();
        persons.insert(
            person_id.clone(),
            InternalPerson::new(person_id.clone(), plan),
        );
        let mut scenario = scenario_with_parts(
            sequential_network(2, None),
            Garage::default(),
            Population { persons },
            Config::default(),
        );

        prepare_for_mobsim(&mut scenario, &empty_router()).unwrap();

        let plan = scenario.population.persons[&person_id]
            .selected_plan()
            .unwrap();
        let legs = plan.legs();
        assert_eq!(Some(travel_time), legs[0].trav_time);
        assert_eq!(
            Some(travel_time),
            legs[0].route.as_ref().unwrap().as_generic().trav_time()
        );
    }

    #[deterministic_id_test]
    fn copies_travel_time_from_leg_to_route() {
        let travel_time = Duration::from_secs(7);
        let plan = routed_plan("walk", Some(travel_time), None);
        let person_id = Id::create("person-1");
        let mut persons = IntMap::default();
        persons.insert(
            person_id.clone(),
            InternalPerson::new(person_id.clone(), plan),
        );
        let mut scenario = scenario_with_parts(
            sequential_network(2, None),
            Garage::default(),
            Population { persons },
            Config::default(),
        );

        prepare_for_mobsim(&mut scenario, &empty_router()).unwrap();

        let plan = scenario.population.persons[&person_id]
            .selected_plan()
            .unwrap();
        let legs = plan.legs();
        assert_eq!(Some(travel_time), legs[0].trav_time);
        assert_eq!(
            Some(travel_time),
            legs[0].route.as_ref().unwrap().as_generic().trav_time()
        );
    }

    #[deterministic_id_test]
    fn adds_travel_distance_to_network_routes() {
        let network = sequential_network(3, Some("car"));
        let mut plan = unrouted_plan("car", "link-1", "link-3", 10);
        let generic_route = InternalGenericRoute::new(
            Id::get_from_ext("link-1"),
            Id::get_from_ext("link-3"),
            None,
            None,
            None,
        );
        plan.elements[1].as_leg_mut().unwrap().route =
            Some(InternalRoute::Network(InternalNetworkRoute::new(
                generic_route,
                vec![
                    Id::get_from_ext("link-1"),
                    Id::get_from_ext("link-2"),
                    Id::get_from_ext("link-3"),
                ],
            )));
        let span = get_trip_spans_default(&plan.elements)[0];
        let mut working_plan = Cow::Borrowed(&plan);

        add_travel_distance(&network, span, &mut working_plan);

        let route = working_plan.elements[1]
            .as_leg()
            .unwrap()
            .route
            .as_ref()
            .unwrap();
        assert_eq!(Some(20.0), route.as_generic().distance());
    }

    #[deterministic_id_test]
    fn leaves_plans_without_network_routes_borrowed() {
        let network = sequential_network(2, None);
        let plan = routed_plan("walk", Some(Duration::ZERO), Some(Duration::ZERO));
        let span = get_trip_spans_default(&plan.elements)[0];
        let mut working_plan = Cow::Borrowed(&plan);

        add_travel_distance(&network, span, &mut working_plan);

        assert!(matches!(working_plan, Cow::Borrowed(_)));
        assert_eq!(
            Some(20.0),
            working_plan.elements[1]
                .as_leg()
                .unwrap()
                .route
                .as_ref()
                .unwrap()
                .as_generic()
                .distance()
        );
    }

    fn routed_plan(
        mode: &str,
        leg_travel_time: Option<Duration>,
        route_travel_time: Option<Duration>,
    ) -> InternalPlan {
        let mut plan = unrouted_plan(mode, "link-1", "link-2", 10);
        let leg = plan.elements[1].as_leg_mut().unwrap();
        leg.trav_time = leg_travel_time;
        leg.route = Some(InternalRoute::Generic(InternalGenericRoute::new(
            Id::get_from_ext("link-1"),
            Id::get_from_ext("link-2"),
            route_travel_time,
            Some(20.0),
            None,
        )));
        plan
    }

    // Before: act--unrouted car--act; after: act--walk--car--walk--act with interaction activities.
    #[deterministic_id_test]
    fn repairs_network_trip_with_access_egress_vehicle_and_routing_mode() {
        let departures = Arc::new(Mutex::new(Vec::new()));
        let router = network_test_router(departures.clone());
        let mut config = Config::default();
        config.qsim_mut().main_modes = vec!["car".to_string()];
        let mut garage = Garage::default();
        garage.add_veh(test_vehicle("person-1_car"));
        let person_id = Id::create("person-1");
        let mut persons = IntMap::default();
        persons.insert(
            person_id.clone(),
            InternalPerson::new(
                person_id.clone(),
                unrouted_plan("car", "link-1", "link-2", 10),
            ),
        );
        let mut scenario = scenario_with_parts(
            sequential_network(2, Some("car")),
            garage,
            Population { persons },
            config,
        );

        prepare_for_mobsim(&mut scenario, &router).unwrap();

        let plan = scenario
            .population
            .persons
            .get(&person_id)
            .unwrap()
            .selected_plan()
            .unwrap();
        let legs = plan.legs();
        assert_eq!(vec!["walk", "car", "walk"], leg_modes(plan));
        assert!(
            legs.iter()
                .all(|leg| leg.routing_mode.as_ref().unwrap().external() == "car")
        );
        let network_route = legs[1].route.as_ref().unwrap().as_network().unwrap();
        assert_eq!(
            "person-1_car",
            network_route
                .generic_delegate()
                .vehicle()
                .as_ref()
                .unwrap()
                .external()
        );
        assert_eq!(vec![SimTime::from_secs(10)], *departures.lock().unwrap());

        // reset travel times
        {
            let person = scenario.population.persons.get_mut(&person_id).unwrap();
            let main_leg = person
                .selected_plan_mut()
                .legs_mut()
                .into_iter()
                .find(|leg| leg.mode.external() == "car")
                .unwrap();
            main_leg.trav_time = None;
            main_leg
                .route
                .as_mut()
                .unwrap()
                .as_generic_mut()
                .set_trav_time(None);
        }

        prepare_for_mobsim(&mut scenario, &router).unwrap();

        assert_eq!(
            vec![SimTime::from_secs(10)],
            *departures.lock().unwrap(),
            "missing travel times on the main-mode leg must not trigger routing"
        );
        let plan = scenario.population.persons[&person_id]
            .selected_plan()
            .unwrap();
        let main_leg = plan
            .legs()
            .into_iter()
            .find(|leg| leg.mode.external() == "car")
            .unwrap();
        assert_eq!(None, main_leg.trav_time);
        assert_eq!(
            None,
            main_leg.route.as_ref().unwrap().as_generic().trav_time()
        );
    }

    // Before: act--unrouted car--act--unrouted car--act; after: both trips are valid access-car-egress chains.
    #[deterministic_id_test]
    fn routes_trips_sequentially_using_prepared_plan_times() {
        let departures = Arc::new(Mutex::new(Vec::new()));
        let router = network_test_router(departures.clone());
        let mut config = Config::default();
        config.qsim_mut().main_modes = vec!["car".to_string()];
        let mut garage = Garage::default();
        garage.add_veh(test_vehicle("person-1_car"));
        let person_id = Id::create("person-1");
        let mut plan = unrouted_plan("car", "link-1", "link-2", 10);
        plan.elements
            .push(InternalPlanElement::Leg(unrouted_leg("car")));
        plan.elements
            .push(InternalPlanElement::Activity(InternalActivity::new(
                Some(Coordinate::new_2d(30.0, 0.0)),
                "shop",
                Id::create("link-3"),
                None,
                None,
                None,
            )));
        let work = plan.elements[2].as_activity().unwrap().clone();
        plan.elements[2] = InternalPlanElement::Activity(InternalActivity {
            max_dur: Some(Duration::from_secs(5)),
            ..work
        });
        let mut persons = IntMap::default();
        persons.insert(
            person_id.clone(),
            InternalPerson::new(person_id.clone(), plan),
        );
        let mut scenario = scenario_with_parts(
            sequential_network(3, Some("car")),
            garage,
            Population { persons },
            config,
        );

        prepare_for_mobsim(&mut scenario, &router).unwrap();

        assert_eq!(
            vec![SimTime::from_secs(10), SimTime::from_secs(19)],
            *departures.lock().unwrap()
        );
    }

    // Before: act--unrouted car--act without a vehicle; after: preparation fails and the plan is unchanged.
    #[deterministic_id_test]
    fn missing_default_vehicle_is_reported_without_calling_router() {
        let departures = Arc::new(Mutex::new(Vec::new()));
        let router = network_test_router(departures.clone());
        let mut config = Config::default();
        config.qsim_mut().main_modes = vec!["car".to_string()];
        let plan = unrouted_plan("car", "link-1", "link-2", 10);
        let original = plan.clone();
        let person_id = Id::create("person-1");
        let mut persons = IntMap::default();
        persons.insert(
            person_id.clone(),
            InternalPerson::new(person_id.clone(), plan),
        );
        let mut scenario = scenario_with_parts(
            sequential_network(2, Some("car")),
            Garage::default(),
            Population { persons },
            config,
        );

        let error = prepare_for_mobsim(&mut scenario, &router).unwrap_err();

        assert!(error.issues()[0].message.contains("person-1_car"));
        assert!(departures.lock().unwrap().is_empty());
        assert_eq!(
            &original,
            scenario.population.persons[&person_id]
                .selected_plan()
                .unwrap()
        );
    }

    // Before: act without an end time--unrouted car--act; after: preparation fails and the plan is unchanged.
    #[deterministic_id_test]
    fn missing_departure_time_is_reported_without_calling_router_or_replacing_plan() {
        let departures = Arc::new(Mutex::new(Vec::new()));
        let router = network_test_router(departures.clone());
        let mut config = Config::default();
        config.qsim_mut().main_modes = vec!["car".to_string()];
        let mut garage = Garage::default();
        garage.add_veh(test_vehicle("person-1_car"));
        let mut plan = unrouted_plan("car", "link-1", "link-2", 10);
        let InternalPlanElement::Activity(origin) = &mut plan.elements[0] else {
            unreachable!()
        };
        origin.end_time = None;
        let original = plan.clone();
        let person_id = Id::create("person-1");
        let mut persons = IntMap::default();
        persons.insert(
            person_id.clone(),
            InternalPerson::new(person_id.clone(), plan),
        );
        let mut scenario = scenario_with_parts(
            sequential_network(2, Some("car")),
            garage,
            Population { persons },
            config,
        );

        let error = prepare_for_mobsim(&mut scenario, &router).unwrap_err();

        assert_eq!(Some(0), error.issues()[0].trip_index);
        assert!(error.issues()[0].message.contains("departure time"));
        assert!(departures.lock().unwrap().is_empty());
        assert_eq!(
            &original,
            scenario.population.persons[&person_id]
                .selected_plan()
                .unwrap()
        );
    }

    // Before: act on a bike-only link--unrouted car--act on a car link; after: the car leg starts on
    // the nearest car link and ends on the destination's own link, which allows car.
    #[deterministic_id_test]
    fn routes_activities_without_facility_with_base_link_first() {
        let plan = route_activities_without_facility(ModalLinkSelection::BaseLinkFirst);

        let car_route = plan.legs()[1].route.as_ref().unwrap().as_generic().clone();
        assert_eq!("car-bike-20", car_route.start_link().external());
        // The destination link allows car, so it is kept although car-bike-20 is nearer.
        assert_eq!("car-0", car_route.end_link().external());
    }

    // Before: as above; after: both ends of the car leg are the nearest car links.
    #[deterministic_id_test]
    fn routes_activities_without_facility_with_nearest_link() {
        let plan = route_activities_without_facility(ModalLinkSelection::NearestLink);

        let car_route = plan.legs()[1].route.as_ref().unwrap().as_generic().clone();
        assert_eq!("car-bike-20", car_route.start_link().external());
        // The destination link allows car, but car-bike-20 is nearer.
        assert_eq!("car-bike-20", car_route.end_link().external());
    }

    /// Routes act on bike-10--unrouted car--act on car-0, with both activities at (50, 19), i.e.
    /// next to car-bike-20, and returns the prepared plan.
    fn route_activities_without_facility(selection: ModalLinkSelection) -> InternalPlan {
        let departures = Arc::new(Mutex::new(Vec::new()));
        let router = network_test_router(departures.clone());
        let mut config = Config::default();
        config.qsim_mut().main_modes = vec!["car".to_string()];
        config.facilities_mut().modal_link_selection = selection;
        let mut garage = Garage::default();
        garage.add_veh(test_vehicle("person-1_car"));
        let network = layered_network();
        let mut plan = InternalPlan::default();
        let mut home =
            located_activity(Some("bike-10"), Some(Coordinate::new_2d(50.0, 19.0)), None);
        home.end_time = Some(SimTime::from_secs(10));
        plan.add_act(home);
        plan.add_leg(unrouted_leg("car"));
        plan.add_act(located_activity(
            Some("car-0"),
            Some(Coordinate::new_2d(50.0, 19.0)),
            None,
        ));
        let person_id = Id::create("person-1");
        let mut persons = IntMap::default();
        persons.insert(
            person_id.clone(),
            InternalPerson::new(person_id.clone(), plan),
        );
        let mut scenario = scenario_with_parts(network, garage, Population { persons }, config);

        prepare_for_mobsim(&mut scenario, &router).unwrap();

        let plan = scenario.population.persons[&person_id]
            .selected_plan()
            .unwrap()
            .clone();
        assert_eq!(vec!["walk", "car", "walk"], leg_modes(&plan));
        // The activities stay on their own links in both cases.
        assert_eq!("bike-10", plan.acts()[0].link_id().external());
        assert_eq!("car-0", plan.acts()[3].link_id().external());
        plan
    }

    // Before: act@facility--unrouted car--act@facility; after: the car leg runs between the modal
    // links, while the activities stay on the facilities' base links.
    #[deterministic_id_test]
    fn routes_trips_between_facilities_via_modal_links() {
        let departures = Arc::new(Mutex::new(Vec::new()));
        let router = network_test_router(departures.clone());
        let mut config = Config::default();
        config.qsim_mut().main_modes = vec!["car".to_string()];
        let mut garage = Garage::default();
        garage.add_veh(test_vehicle("person-1_car"));
        let network = layered_network();
        let facilities = facilities(vec![
            activity_facility("home", 50.0, 19.0, Some("bike-10")),
            activity_facility("work", 50.0, 1.0, None),
        ]);
        let mut plan = InternalPlan::default();
        let mut home = located_activity(None, None, Some("home"));
        home.end_time = Some(SimTime::from_secs(10));
        plan.add_act(home);
        plan.add_leg(unrouted_leg("car"));
        plan.add_act(located_activity(None, None, Some("work")));
        let person_id = Id::create("person-1");
        let mut persons = IntMap::default();
        persons.insert(
            person_id.clone(),
            InternalPerson::new(person_id.clone(), plan),
        );
        let mut scenario = Scenario {
            network,
            garage,
            population: Population { persons },
            transit_schedule: TransitSchedule::default(),
            facilities,
            config: Arc::new(config),
            signals: Default::default(),
        };
        prepare_for_sim(&mut scenario).unwrap();
        let mut scenario: ControllerScenario = scenario.into();

        prepare_for_mobsim(&mut scenario, &router).unwrap();

        let plan = scenario.population.persons[&person_id]
            .selected_plan()
            .unwrap();
        assert_eq!(vec!["walk", "car", "walk"], leg_modes(plan));
        assert_eq!("bike-10", plan.acts()[0].link_id().external());
        assert_eq!("car-0", plan.acts()[3].link_id().external());
        let access_interaction = plan.acts()[1];
        assert_eq!("car-bike-20", access_interaction.link_id().external());
        let car_route = plan.legs()[1].route.as_ref().unwrap().as_generic().clone();
        assert_eq!("car-bike-20", car_route.start_link().external());
        assert_eq!("car-0", car_route.end_link().external());
        assert_eq!(vec![SimTime::from_secs(10)], *departures.lock().unwrap());
    }

    fn scenario_with_population(population: Population) -> ControllerScenario {
        scenario_with_network_and_population(Network::new(), population)
    }

    fn empty_router() -> TripRouter {
        TripRouter::new(IntMap::default())
    }

    fn scenario_with_network_and_population(
        network: Network,
        population: Population,
    ) -> ControllerScenario {
        Scenario {
            network,
            garage: Garage::default(),
            population,
            transit_schedule: TransitSchedule::default(),
            facilities: ActivityFacilities::default(),
            config: Arc::new(Config::default()),
            signals: Signals::default(),
        }
        .into()
    }

    fn scenario_with_parts(
        network: Network,
        garage: Garage,
        population: Population,
        config: Config,
    ) -> ControllerScenario {
        Scenario {
            network,
            garage,
            population,
            transit_schedule: TransitSchedule::default(),
            facilities: ActivityFacilities::default(),
            config: Arc::new(config),
            signals: Signals::default(),
        }
        .into()
    }

    fn network_with_link(link_id: Id<Link>) -> Network {
        let mut network = Network::new();
        let from = Node::new(
            Id::create("from-node"),
            Coordinate::new_3d(0.0, 10.0, 4.0),
            0,
            1,
        );
        let to = Node::new(
            Id::create("to-node"),
            Coordinate::new_3d(10.0, 20.0, 16.0),
            0,
            1,
        );
        let link = Link::new_with_default(link_id, &from, &to);

        network.add_node(from);
        network.add_node(to);
        network.add_link(link);
        network
    }

    fn person(id: &str, link_id: &str) -> InternalPerson {
        let mut plan = InternalPlan::default();
        plan.add_act(InternalActivity::new(
            Some(Coordinate::default()),
            "act",
            Id::create(link_id),
            None,
            None,
            None,
        ));
        InternalPerson::new(Id::create(id), plan)
    }

    fn teleportation_router(mode: &str) -> TripRouter {
        let mut modules: IntMap<Id<String>, Arc<dyn RoutingModule>> = IntMap::default();
        let mode_id = Id::create(mode);
        modules.insert(
            mode_id.clone(),
            Arc::new(TeleportationRoutingModule::new(mode_id, 1.0, 1.0)),
        );
        TripRouter::new(modules)
    }

    fn network_test_router(departures: Arc<Mutex<Vec<SimTime>>>) -> TripRouter {
        let mut modules: IntMap<Id<String>, Arc<dyn RoutingModule>> = IntMap::default();
        let mode = Id::create("car");
        modules.insert(
            mode.clone(),
            Arc::new(TestNetworkRoutingModule { mode, departures }),
        );
        TripRouter::new(modules)
    }

    fn unrouted_plan(mode: &str, from: &str, to: &str, departure: u64) -> InternalPlan {
        let mut plan = InternalPlan::default();
        plan.add_act(InternalActivity::new(
            Some(Coordinate::new_2d(0.0, 0.0)),
            "home",
            Id::create(from),
            None,
            Some(SimTime::from_secs(departure)),
            None,
        ));
        plan.elements
            .push(InternalPlanElement::Leg(unrouted_leg(mode)));
        plan.add_act(InternalActivity::new(
            Some(Coordinate::new_2d(20.0, 0.0)),
            "work",
            Id::create(to),
            None,
            None,
            None,
        ));
        plan
    }

    fn unrouted_leg(mode: &str) -> InternalLeg {
        InternalLeg {
            mode: Id::create(mode),
            routing_mode: Some(Id::create(mode)),
            dep_time: None,
            trav_time: None,
            route: None,
            attributes: InternalAttributes::default(),
        }
    }

    fn sequential_network(link_count: usize, mode: Option<&str>) -> Network {
        let mut network = Network::new();
        let nodes: Vec<_> = (0..=link_count)
            .map(|index| {
                Node::new(
                    Id::create(&format!("node-{index}")),
                    Coordinate::new_2d(index as f64 * 10.0, 0.0),
                    0,
                    1,
                )
            })
            .collect();
        for node in &nodes {
            network.add_node(node.clone());
        }
        for index in 0..link_count {
            let mut modes = IntSet::default();
            if let Some(mode) = mode {
                modes.insert(Id::create(mode));
            }
            network.add_link(Link::new(
                Id::create(&format!("link-{}", index + 1)),
                nodes[index].id.clone(),
                nodes[index + 1].id.clone(),
                10.0,
                1.0,
                1.0,
                1.0,
                modes,
                0,
            ));
        }
        network
    }

    fn test_vehicle(id: &str) -> InternalVehicle {
        InternalVehicle {
            id: Id::create(id),
            max_v: 10.0,
            pce: 1.0,
            vehicle_type: Id::<InternalVehicleType>::create("car"),
            attributes: InternalAttributes::default(),
        }
    }

    fn leg_modes(plan: &InternalPlan) -> Vec<&str> {
        plan.legs()
            .into_iter()
            .map(|leg| leg.mode.external())
            .collect()
    }

    /// Dummy routing module that stores the departure times of the requests and returns a walk (0s) -> car (10s) -> walk (0s) trip.
    struct TestNetworkRoutingModule {
        mode: Id<String>,
        departures: Arc<Mutex<Vec<SimTime>>>,
    }

    impl RoutingModule for TestNetworkRoutingModule {
        fn calc_route(
            &self,
            request: RoutingRequest,
        ) -> Result<Vec<InternalPlanElement>, RoutingError> {
            self.departures
                .lock()
                .unwrap()
                .push(request.departure_time());
            let from = request.from().modal_link(&self.mode).clone();
            let to = request.to().modal_link(&self.mode).clone();
            let one_second = Duration::from_secs(1);
            let two_seconds = Duration::from_secs(2);

            let access_route = InternalRoute::Generic(InternalGenericRoute::new(
                from.clone(),
                from.clone(),
                Some(one_second),
                Some(0.0),
                None,
            ));
            let access = InternalPlanElement::Leg(InternalLeg::new(
                access_route,
                "walk",
                "walk",
                one_second,
                Some(request.departure_time()),
            ));
            let access_interaction = InternalPlanElement::Activity(InternalActivity::new(
                Some(request.from().coord().clone()),
                "car interaction",
                from.clone(),
                None,
                None,
                Some(Duration::ZERO),
            ));

            let network_generic = InternalGenericRoute::new(
                from.clone(),
                to.clone(),
                Some(two_seconds),
                Some(10.0),
                request.vehicle().map(|vehicle| vehicle.id().clone()),
            );
            let network_route = InternalRoute::Network(InternalNetworkRoute::new(
                network_generic,
                vec![from.clone(), to.clone()],
            ));
            let network_leg = InternalPlanElement::Leg(InternalLeg::new(
                network_route,
                "car",
                "car",
                two_seconds,
                None,
            ));
            let egress_interaction = InternalPlanElement::Activity(InternalActivity::new(
                Some(request.to().coord().clone()),
                "car interaction",
                to.clone(),
                None,
                None,
                Some(Duration::ZERO),
            ));
            let egress_route = InternalRoute::Generic(InternalGenericRoute::new(
                to.clone(),
                to,
                Some(one_second),
                Some(0.0),
                None,
            ));
            let egress = InternalPlanElement::Leg(InternalLeg::new(
                egress_route,
                "walk",
                "walk",
                one_second,
                None,
            ));

            Ok(vec![
                access,
                access_interaction,
                network_leg,
                egress_interaction,
                egress,
            ])
        }

        fn mode(&self) -> &Id<String> {
            &self.mode
        }
    }

    fn assert_send_sync<T: Send + Sync>() {}

    // No plan is built or changed; this compile-time test only checks that TripRouter is Send + Sync.
    #[test]
    fn trip_router_is_send_and_sync() {
        assert_send_sync::<TripRouter>();
    }
}
