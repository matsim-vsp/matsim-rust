use crate::simulation::id::Id;
use crate::simulation::replanning::routing::{RoutingError, RoutingModule, RoutingRequest};
use crate::simulation::scenario::Coordinate;
use crate::simulation::scenario::population::{
    InternalGenericRoute, InternalLeg, InternalPlanElement, InternalRoute,
};
use std::time::Duration;

pub struct TeleportationRoutingModule {
    mode: Id<String>,
    beeline_distance_factor: f64,
    travel_speed: f64,
}

impl RoutingModule for TeleportationRoutingModule {
    fn calc_route(
        &self,
        request: RoutingRequest,
    ) -> Result<Vec<InternalPlanElement>, RoutingError> {
        let mode = self.mode.external();
        let dep_time = Some(request.departure_time);

        let start = request.from.modal_link(&self.mode).clone();
        let end = request.to.modal_link(&self.mode).clone();

        let from_coord = request.from.coord();
        let to_coord = request.to.coord();

        let distance =
            Coordinate::euclidean_distance(from_coord, to_coord) * self.beeline_distance_factor;

        let trav_time = Duration::from_secs_f64(distance / self.travel_speed);
        let route = InternalRoute::Generic(InternalGenericRoute::new(
            start,
            end,
            Some(trav_time),
            Some(distance),
            request.vehicle.map(|v| v.id().clone()),
        ));

        let leg = InternalLeg::new(route, mode, mode, trav_time, dep_time);
        Ok(vec![InternalPlanElement::Leg(leg)])
    }

    fn mode(&self) -> &Id<String> {
        &self.mode
    }
}

impl TeleportationRoutingModule {
    pub fn new(mode: Id<String>, beeline_distance_factor: f64, travel_speed: f64) -> Self {
        Self {
            mode,
            beeline_distance_factor,
            travel_speed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::TeleportationRoutingModule;
    use crate::simulation::InternalAttributes;
    use crate::simulation::id::Id;
    use crate::simulation::replanning::routing::{
        Facility, RoutingModule, RoutingRequest, RoutingRequestBuilder,
    };
    use crate::simulation::scenario::Coordinate;
    use crate::simulation::scenario::facilities::ActivityFacility;
    use crate::simulation::scenario::network::Link;
    use crate::simulation::scenario::population::{InternalPlanElement, InternalRoute};
    use crate::simulation::time::SimTime;
    use assert_approx_eq::assert_approx_eq;
    use macros::deterministic_id_test;
    use nohash_hasher::IntMap;
    use std::time::Duration;

    #[deterministic_id_test]
    fn calc_route_returns_single_generic_leg() {
        let module = TeleportationRoutingModule::new(Id::create("walk"), 1.3, 2.0);
        let from = facility("from", 0.0, 0.0, "from-link", []);
        let to = facility("to", 3.0, 4.0, "to-link", []);
        let (from, to) = (
            Facility::ActivityFacility(&from),
            Facility::ActivityFacility(&to),
        );
        let departure_time = SimTime::from_secs(42);

        let plan = module
            .calc_route(request(&from, &to, departure_time))
            .unwrap();

        assert_eq!(1, plan.len());
        let InternalPlanElement::Leg(leg) = &plan[0] else {
            panic!("Expected a single leg");
        };

        let expected_distance = 6.5;
        let expected_travel_time = Duration::from_secs_f64(expected_distance / 2.0);

        assert_eq!("walk", leg.mode.external());
        assert_eq!(Some(Id::create("walk")), leg.routing_mode);
        assert_eq!(Some(departure_time), leg.dep_time);
        assert_eq!(Some(expected_travel_time), leg.trav_time);

        let route = leg.route.as_ref().expect("Leg must have a route");
        assert!(matches!(route, InternalRoute::Generic(_)));
        let generic_route = route.as_generic();

        assert_eq!("from-link", generic_route.start_link().external());
        assert_eq!("to-link", generic_route.end_link().external());
        assert_eq!(Some(expected_travel_time), generic_route.trav_time());
        assert_approx_eq!(expected_distance, generic_route.distance().unwrap());
        assert_eq!(&None, generic_route.vehicle());
    }

    #[deterministic_id_test]
    fn calc_route_prefers_mode_specific_links() {
        let module = teleportation_module("walk", 1.0, 1.0);
        let from = facility(
            "from",
            0.0,
            0.0,
            "from-base-link",
            [("walk", "from-walk-link")],
        );
        let to = facility("to", 0.0, 1.0, "to-base-link", [("walk", "to-walk-link")]);
        let (from, to) = (
            Facility::ActivityFacility(&from),
            Facility::ActivityFacility(&to),
        );

        let plan = module
            .calc_route(request(&from, &to, SimTime::from_secs(0)))
            .unwrap();

        let InternalPlanElement::Leg(leg) = &plan[0] else {
            panic!("Expected a single leg");
        };
        let route = leg.route.as_ref().expect("Leg must have a route");
        let generic_route = route.as_generic();

        assert_eq!("from-walk-link", generic_route.start_link().external());
        assert_eq!("to-walk-link", generic_route.end_link().external());
    }

    fn teleportation_module(
        mode: &str,
        beeline_distance_factor: f64,
        travel_speed: f64,
    ) -> TeleportationRoutingModule {
        TeleportationRoutingModule {
            mode: Id::create(mode),
            beeline_distance_factor,
            travel_speed,
        }
    }

    fn request<'a>(
        from: &'a Facility<'a>,
        to: &'a Facility<'a>,
        departure_time: SimTime,
    ) -> RoutingRequest<'a> {
        RoutingRequestBuilder::default()
            .from(from)
            .to(to)
            .departure_time(departure_time)
            .build()
            .unwrap()
    }

    fn facility<const N: usize>(
        id: &str,
        x: f64,
        y: f64,
        base_link: &str,
        mode_links: [(&str, &str); N],
    ) -> ActivityFacility {
        let mut mode_to_link = IntMap::default();
        for (mode, link) in mode_links {
            mode_to_link.insert(Id::create(mode), Id::<Link>::create(link));
        }

        ActivityFacility {
            id: Id::create(id),
            coord: Coordinate::new_2d(x, y),
            base_link: Some(Id::create(base_link)),
            mode_to_link,
            desc: None,
            activities: Vec::new(),
            attributes: InternalAttributes::default(),
        }
    }
}
