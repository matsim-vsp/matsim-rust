use crate::simulation::id::Id;
use crate::simulation::replanning::routing::least_cost_path_calculator::{
    LeastCostPath, LeastCostPathCalculator, LeastCostPathRequestBuilder,
};
use crate::simulation::replanning::routing::{
    Facility, RoutingError, RoutingModule, RoutingRequest, RoutingRequestBuilder,
};
use crate::simulation::scenario::network::Link;
use crate::simulation::scenario::population::{
    InternalActivity, InternalGenericRoute, InternalLeg, InternalNetworkRoute, InternalPlanElement,
    InternalRoute,
};
use crate::simulation::scenario::vehicles::InternalVehicle;
use crate::simulation::scenario::{Coordinate, ScenarioCore, network};
use crate::simulation::time::SimTime;
use crate::simulation::time::time_interpretation::TimeInterpretation;
use std::sync::Arc;
use std::time::Duration;

pub struct NetworkRoutingModule {
    mode: Id<String>,
    access_router: Arc<dyn RoutingModule>,
    egress_router: Arc<dyn RoutingModule>,
    least_cost_path_calculator: Box<dyn LeastCostPathCalculator>,
    scenario: ScenarioCore,
}

impl RoutingModule for NetworkRoutingModule {
    fn calc_route(
        &self,
        request: RoutingRequest,
    ) -> Result<Vec<InternalPlanElement>, RoutingError> {
        let mut result = Vec::with_capacity(5);

        let access_link = request.from.modal_link(&self.mode).clone();
        let egress_link = request.to.modal_link(&self.mode).clone();

        // ====== route access leg
        let mut now = self.access_routing(&request, &access_link, &mut result)?;

        // ====== route "true" leg
        now = self.network_leg(&request, &access_link, &egress_link, now, &mut result)?;

        // ======= route egress leg
        self.egress_routing(&request, &egress_link, now, &mut result)?;

        Ok(result)
    }

    fn mode(&self) -> &Id<String> {
        &self.mode
    }
}

impl NetworkRoutingModule {
    pub fn new(
        mode: Id<String>,
        access_egress: Arc<dyn RoutingModule>,
        least_cost_path_calculator: Box<dyn LeastCostPathCalculator>,
        scenario: ScenarioCore,
    ) -> Self {
        NetworkRoutingModule {
            mode,
            access_router: access_egress.clone(),
            egress_router: access_egress,
            least_cost_path_calculator,
            scenario,
        }
    }

    /// Calculates the legs needed from activity --> access link
    fn access_routing(
        &self,
        original_request: &RoutingRequest,
        access_link: &Id<Link>,
        result: &mut Vec<InternalPlanElement>,
    ) -> Result<SimTime, RoutingError> {
        let coord = network::utils::find_nearest_point_on_link(
            original_request.from.coord(),
            access_link,
            self.scenario.network.as_ref(),
        );
        let to = Facility::new_link_wrapper(coord.clone(), access_link.clone());

        let new_req = RoutingRequestBuilder::default()
            .from(original_request.from)
            .to(&to)
            .departure_time(original_request.departure_time)
            .attributes(original_request.attributes.clone())
            .person(original_request.person)
            .build()
            .unwrap();

        let access = self.access_router.calc_route(new_req)?;
        let now = TimeInterpretation::decide_on_elements_end_time(
            &access,
            &original_request.departure_time,
        )
        .ok_or_else(|| RoutingError::MissingEndTime {
            mode: self.mode.external().to_string(),
        })?;
        result.extend(access);
        let interaction_activity = self.create_interaction_activity(coord, access_link);
        result.push(interaction_activity);

        Ok(now)
    }

    /// Calculates the legs needed from egress link --> activity
    fn egress_routing(
        &self,
        original_request: &RoutingRequest,
        egress_link: &Id<Link>,
        now: SimTime,
        result: &mut Vec<InternalPlanElement>,
    ) -> Result<(), RoutingError> {
        let coord = network::utils::find_nearest_point_on_link(
            original_request.to.coord(),
            egress_link,
            self.scenario.network.as_ref(),
        );
        let from = Facility::new_link_wrapper(coord.clone(), egress_link.clone());

        let new_req = RoutingRequestBuilder::default()
            .from(&from)
            .to(original_request.to)
            .departure_time(now)
            .attributes(original_request.attributes.clone())
            .person(original_request.person)
            .build()
            .unwrap();

        let interaction_activity = self.create_interaction_activity(coord, egress_link);
        result.push(interaction_activity);
        let egress = self.egress_router.calc_route(new_req)?;
        result.extend(egress);
        Ok(())
    }

    /// Calculates the legs needed from access link --> egress link
    fn network_leg(
        &self,
        request: &RoutingRequest,
        from: &Id<Link>,
        to: &Id<Link>,
        now: SimTime,
        result: &mut Vec<InternalPlanElement>,
    ) -> Result<SimTime, RoutingError> {
        let person = request.person;

        let path = if from == to {
            LeastCostPath {
                path: Vec::new(),
                travel_time: Duration::from_secs(0),
                travel_disutility: 0.0,
            }
        } else {
            let r = LeastCostPathRequestBuilder::default()
                .from(from.clone())
                .to(to.clone())
                .person(person)
                .vehicle(request.vehicle)
                .candidate_path(request.candidate_path().map(<[_]>::to_vec))
                .departure_time(now)
                .build()
                .unwrap();

            self.least_cost_path_calculator
                .calc_least_cost_path(r)
                .ok_or_else(|| RoutingError::NoPath {
                    mode: self.mode.external().to_string(),
                    from: from.external().to_string(),
                    to: to.external().to_string(),
                })?
        };

        let elements =
            vec![self.path_to_elements(path, from, to, &self.mode, request.vehicle, now)];
        let time =
            TimeInterpretation::decide_on_elements_end_time(&elements, &now).ok_or_else(|| {
                RoutingError::MissingEndTime {
                    mode: self.mode.external().to_string(),
                }
            })?;
        result.extend(elements);
        Ok(time)
    }

    fn path_to_elements(
        &self,
        path: LeastCostPath,
        from: &Id<Link>,
        to: &Id<Link>,
        mode: &Id<String>,
        vehicle: Option<&InternalVehicle>,
        now: SimTime,
    ) -> InternalPlanElement {
        if from == to {
            // If from and to are the same, we build a dummy node.
            let generic = InternalGenericRoute::new(
                from.clone(),
                to.clone(),
                Some(Duration::from_nanos(0)),
                Some(0.),
                vehicle.map(|v| v.id().clone()),
            );
            let net_route = InternalNetworkRoute::new(generic, vec![from.clone()]);
            return InternalPlanElement::Leg(InternalLeg::new(
                InternalRoute::Network(net_route),
                mode.external(),
                mode.external(),
                Duration::from_secs(0),
                Some(now),
            ));
        }

        let mut route = Vec::with_capacity(path.path.len() + 2);
        route.push(from.clone());
        if from != to {
            route.extend(path.path);
            route.push(to.clone());
        }

        let start = route.first().unwrap().clone();
        let end = route.last().unwrap().clone();

        // Correct travel time: the path doesn't contain the last link. Manually added here.
        let to_link = self.scenario.network.get_link(to);
        let vehicle_speed = if let Some(v) = vehicle {
            v.max_v
        } else {
            f64::MAX
        };
        let max_speed = to_link.freespeed.min(vehicle_speed);
        let trav_time = path.travel_time + Duration::from_secs_f64(to_link.length / max_speed);

        // calculate the distance of the path. Ignore the first link (since vehicle starts at the very end).
        let distance: f64 = route[1..]
            .iter()
            .map(|link| self.scenario.network.get_link(link))
            .map(|l| l.length)
            .sum();

        let generic = InternalGenericRoute::new(
            start,
            end,
            Some(trav_time),
            Some(distance),
            vehicle.map(|v| v.id().clone()),
        );

        let net_route = InternalNetworkRoute::new(generic, route);
        let route = InternalRoute::Network(net_route);
        let leg = InternalLeg::new(
            route,
            mode.external(),
            mode.external(),
            trav_time,
            Some(now),
        );
        InternalPlanElement::Leg(leg)
    }

    fn create_interaction_activity(
        &self,
        coord: Coordinate,
        link: &Id<Link>,
    ) -> InternalPlanElement {
        InternalPlanElement::Activity(InternalActivity::new(
            Some(coord),
            &format!("{} interaction", self.mode.external()),
            link.clone(),
            None,
            None,
            Some(Duration::from_secs(0)),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::NetworkRoutingModule;
    use crate::simulation::InternalAttributes;
    use crate::simulation::config::Config;
    use crate::simulation::id::Id;
    use crate::simulation::network::signals::Signals;
    use crate::simulation::replanning::routing::a_star::Alt;
    use crate::simulation::replanning::routing::cost::FreeSpeedTravelTimeAndDisutility;
    use crate::simulation::replanning::routing::teleportation::TeleportationRoutingModule;
    use crate::simulation::replanning::routing::{Facility, RoutingModule, RoutingRequestBuilder};
    use crate::simulation::scenario::facilities::{ActivityFacilities, ActivityFacility};
    use crate::simulation::scenario::network::{Link, Network};
    use crate::simulation::scenario::population::{
        InternalActivity, InternalLeg, InternalPlanElement, InternalRoute,
    };
    use crate::simulation::scenario::vehicles::Garage;
    use crate::simulation::scenario::{Coordinate, ScenarioCore};
    use crate::simulation::time::SimTime;
    use assert_approx_eq::assert_approx_eq;
    use macros::deterministic_id_test;
    use nohash_hasher::IntMap;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;

    #[deterministic_id_test]
    fn calc_route_with_alt_returns_expected_network_trips() {
        assert_route_with_alt(
            facility("from_20", 2500.0, 100.0, "20"),
            facility("to_1", -17500.0, 200.0, "1"),
            ExpectedTrip {
                access_link: "20",
                access_projection: (2500.0, 0.0),
                access_distance: 100.0,
                egress_link: "1",
                egress_projection: (-17500.0, 0.0),
                egress_distance: 200.0,
                network_start: "20",
                network_end: "1",
                network_distance: 65000.0,
                network_travel_time: None,
                network_routes: &[&["20", "21", "22", "23", "1"]],
            },
        );

        assert_route_with_alt(
            facility("from_1", -17500.0, 100.0, "1"),
            facility("to_20", 2500.0, 200.0, "20"),
            ExpectedTrip {
                access_link: "1",
                access_projection: (-17500.0, 0.0),
                access_distance: 100.0,
                egress_link: "20",
                egress_projection: (2500.0, 0.0),
                egress_distance: 200.0,
                network_start: "1",
                network_end: "20",
                network_distance: 25000.0,
                network_travel_time: None,
                network_routes: &[&["1", "2", "11", "20"]],
            },
        );

        assert_route_with_alt(
            facility("from_20_same_link", 1000.0, 100.0, "20"),
            facility("to_20_same_link", 4000.0, 200.0, "20"),
            ExpectedTrip {
                access_link: "20",
                access_projection: (1000.0, 0.0),
                access_distance: 100.0,
                egress_link: "20",
                egress_projection: (4000.0, 0.0),
                egress_distance: 200.0,
                network_start: "20",
                network_end: "20",
                network_distance: 0.0,
                network_travel_time: Some(Duration::from_secs(0)),
                network_routes: &[&["20"]],
            },
        );
    }

    // The base link is only the facility's address. Access, egress and the network leg use the
    // modal link of the mode.
    #[deterministic_id_test]
    fn calc_route_uses_modal_link_instead_of_base_link() {
        let mut from = facility("from_base_20_modal_1", -17500.0, 100.0, "20");
        from.mode_to_link
            .insert(Id::create("car"), Id::<Link>::create("1"));

        assert_route_with_alt(
            from,
            facility("to_20", 2500.0, 200.0, "20"),
            ExpectedTrip {
                access_link: "1",
                access_projection: (-17500.0, 0.0),
                access_distance: 100.0,
                egress_link: "20",
                egress_projection: (2500.0, 0.0),
                egress_distance: 200.0,
                network_start: "1",
                network_end: "20",
                network_distance: 25000.0,
                network_travel_time: None,
                network_routes: &[&["1", "2", "11", "20"]],
            },
        );
    }

    fn assert_route_with_alt(from: ActivityFacility, to: ActivityFacility, expected: ExpectedTrip) {
        let plan = calc_route_with_alt(
            &Facility::ActivityFacility(&from),
            &Facility::ActivityFacility(&to),
        );

        // Basic structure assertions
        assert_eq!(5, plan.len());

        let access = leg_at(&plan, 0);
        let access_interaction = activity_at(&plan, 1);
        let network = leg_at(&plan, 2);
        let egress_interaction = activity_at(&plan, 3);
        let egress = leg_at(&plan, 4);

        assert_eq!("walk", access.mode.external());
        assert_eq!("car", network.mode.external());
        assert_eq!("walk", egress.mode.external());

        assert_interaction_activity(
            access_interaction,
            expected.access_link,
            expected.access_projection.0,
            expected.access_projection.1,
        );
        assert_interaction_activity(
            egress_interaction,
            expected.egress_link,
            expected.egress_projection.0,
            expected.egress_projection.1,
        );

        // Assert routes
        assert!(matches!(
            access.route.as_ref().expect("Access leg must have a route"),
            InternalRoute::Generic(_)
        ));
        assert!(matches!(
            network
                .route
                .as_ref()
                .expect("Network leg must have a route"),
            InternalRoute::Network(_)
        ));
        assert!(matches!(
            egress.route.as_ref().expect("Egress leg must have a route"),
            InternalRoute::Generic(_)
        ));

        let access_route = leg_at(&plan, 0)
            .route
            .as_ref()
            .expect("Access leg must have a route")
            .as_generic();
        let egress_route = leg_at(&plan, 4)
            .route
            .as_ref()
            .expect("Egress leg must have a route")
            .as_generic();

        // Access and egress walk legs connect the facility's walk link, i.e. its base link, with
        // the car access and egress links.
        let walk = Id::create("walk");
        assert_eq!(
            Facility::ActivityFacility(&from).modal_link(&walk),
            access_route.start_link()
        );
        assert_eq!(expected.access_link, access_route.end_link().external());
        assert_approx_eq!(expected.access_distance, access_route.distance().unwrap());

        assert_eq!(expected.egress_link, egress_route.start_link().external());
        assert_eq!(
            Facility::ActivityFacility(&to).modal_link(&walk),
            egress_route.end_link()
        );
        assert_approx_eq!(expected.egress_distance, egress_route.distance().unwrap());

        let network_leg = network;
        assert_eq!("car", network_leg.mode.external());
        assert_eq!(Some(Id::create("car")), network_leg.routing_mode);
        if let Some(expected_network_travel_time) = expected.network_travel_time {
            assert_eq!(Some(expected_network_travel_time), network_leg.trav_time);
        }

        let route = network_leg
            .route
            .as_ref()
            .expect("Network leg must have a route")
            .as_network()
            .expect("Network leg must carry a network route");

        let link_ids = route
            .route()
            .iter()
            .map(|id| id.external())
            .collect::<Vec<_>>();
        assert!(
            expected
                .network_routes
                .iter()
                .any(|expected_route| link_ids == *expected_route),
            "Unexpected network route: {link_ids:?}",
        );

        let generic = route.generic_delegate();
        assert_eq!(expected.network_start, generic.start_link().external());
        assert_eq!(expected.network_end, generic.end_link().external());
        assert_approx_eq!(expected.network_distance, generic.distance().unwrap());
    }

    fn calc_route_with_alt(from: &Facility, to: &Facility) -> Vec<InternalPlanElement> {
        let network = Arc::new(Network::from_file_as_is(&PathBuf::from(
            "./assets/equil/equil-network.xml",
        )));
        let travel_cost = Arc::new(FreeSpeedTravelTimeAndDisutility {});
        let router = Alt::new(network.clone(), None, travel_cost.clone(), travel_cost).unwrap();

        let least_cost_path_calculator = Box::new(router);
        let module = NetworkRoutingModule::new(
            Id::create("car"),
            Arc::new(TeleportationRoutingModule::new(Id::create("walk"), 1., 1.)),
            least_cost_path_calculator,
            ScenarioCore {
                network,
                garage: Arc::new(Garage::default()),
                transit_schedule: Arc::new(
                    crate::simulation::scenario::transit::TransitSchedule::default(),
                ),
                transit_runs: Default::default(),
                facilities: Arc::new(ActivityFacilities::default()),
                config: Arc::new(Config::default()),
                signals: Arc::new(Signals::default()),
            },
        );

        let request = RoutingRequestBuilder::default()
            .from(from)
            .to(to)
            .departure_time(SimTime::from_secs(0))
            .build()
            .unwrap();

        module.calc_route(request).unwrap()
    }

    fn facility(id: &str, x: f64, y: f64, link_id: &str) -> ActivityFacility {
        ActivityFacility {
            id: Id::create(id),
            coord: Coordinate::new_2d(x, y),
            base_link: Some(Id::<Link>::create(link_id)),
            mode_to_link: IntMap::default(),
            desc: None,
            activities: Vec::new(),
            attributes: InternalAttributes::default(),
        }
    }

    fn leg_at(plan: &[InternalPlanElement], index: usize) -> &InternalLeg {
        let InternalPlanElement::Leg(leg) = &plan[index] else {
            panic!("Expected leg at index {index}");
        };
        leg
    }

    fn activity_at(plan: &[InternalPlanElement], index: usize) -> &InternalActivity {
        let InternalPlanElement::Activity(activity) = &plan[index] else {
            panic!("Expected activity at index {index}");
        };
        activity
    }

    struct ExpectedTrip<'a> {
        access_link: &'a str,
        access_projection: (f64, f64),
        access_distance: f64,
        egress_link: &'a str,
        egress_projection: (f64, f64),
        egress_distance: f64,
        network_start: &'a str,
        network_end: &'a str,
        network_distance: f64,
        network_travel_time: Option<Duration>,
        network_routes: &'a [&'a [&'a str]],
    }

    fn assert_interaction_activity(
        activity: &InternalActivity,
        link_id: &str,
        expected_x: f64,
        expected_y: f64,
    ) {
        assert!(activity.is_interaction());
        assert_eq!("car interaction", activity.act_type.external());
        assert_eq!(link_id, activity.link_id().external());
        assert_eq!(Some(Duration::from_secs(0)), activity.max_dur);

        let coord = activity
            .coord
            .as_ref()
            .expect("Interaction activity must have a coordinate");
        assert_approx_eq!(expected_x, coord.x);
        assert_approx_eq!(expected_y, coord.y);
    }
}
