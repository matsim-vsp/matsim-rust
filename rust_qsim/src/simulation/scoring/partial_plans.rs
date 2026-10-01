use crate::simulation::events::{
    ActivityEndEvent, ActivityStartEvent, EventTrait, LinkEnterEvent, PersonArrivalEvent,
    PersonDepartureEvent, PersonEntersVehicleEvent, PersonStuckEvent, PtTeleportationArrivalEvent,
    TeleportationArrivalEvent, VehicleEntersTrafficEvent, VehicleLeavesTrafficEvent,
};
use crate::simulation::id::Id;
use crate::simulation::scenario::Coordinate;
use crate::simulation::scenario::network::Link;
use crate::simulation::scenario::population::InternalPlanElement::{Activity, Leg};
use crate::simulation::scenario::population::{
    InternalActivity, InternalGenericRoute, InternalLeg, InternalNetworkRoute, InternalPlan,
    InternalPlanElement, InternalPtRoute, InternalPtRouteDescription, InternalRoute,
};
use crate::simulation::scenario::vehicles::InternalVehicle;
use crate::simulation::time::SimTime;
use std::time::Duration;

pub struct PartialPlan {
    elements: Vec<InternalPlanElement>,
    current_activity: Option<PartialActivity>,
    current_leg: Option<PartialLeg>,
}

impl Default for PartialPlan {
    fn default() -> Self {
        Self {
            elements: Vec::default(),
            current_activity: Some(PartialActivity::default()),
            current_leg: None,
        }
    }
}

impl PartialPlan {
    fn handle_person_departure(&mut self) {
        if self.current_leg.is_some() {
            panic!("Illegal state: Person departs while having an active leg!");
        }

        self.current_leg = Some(PartialLeg::default());
    }

    fn handle_person_arrival(&mut self) {
        if self.current_leg.is_none() {
            panic!("Illegal state: Person arrives while having no active leg!");
        }

        self.elements
            .push(Leg(self.current_leg.take().unwrap().finish()));
    }

    fn handle_activity_start(&mut self) {
        if self.current_activity.is_some() {
            panic!("Illegal state: Person starts activity while doing an activity!");
        }

        self.current_activity = Some(PartialActivity::default());
    }

    fn handle_activity_end(&mut self) {
        if self.current_activity.is_none() {
            panic!("Illegal state: Person ends activity while not doing an activity!");
        }

        self.elements
            .push(Activity(self.current_activity.take().unwrap().finish()))
    }

    fn handle_stuck(&mut self) {
        if let Some(act) = &mut self.current_activity {
            // if the current activity is not ended, we mark it as aborted.
            act.aborted = true;
        } else if self.current_leg.is_none() {
            // if also leg is none, we are in between an activity and a leg. This is counted as an aborted activity.
            if let Activity(act) = self.elements.last_mut().unwrap() {
                act.attributes.add("aborted", true);
            } else {
                panic!(
                    "Illegal state: Person is stuck while not doing an activity or leg, but the last plan element is not an activity!"
                );
            }
        }
    }

    pub(crate) fn handle_event(&mut self, event: &dyn EventTrait) {
        if let Some(_) = event.as_any().downcast_ref::<PersonStuckEvent>() {
            self.handle_stuck();
            return;
        }
        if let Some(_) = event.as_any().downcast_ref::<PersonDepartureEvent>() {
            self.handle_person_departure();
        } else if let Some(_) = event.as_any().downcast_ref::<ActivityStartEvent>() {
            self.handle_activity_start();
        }

        if self.current_leg.is_some() {
            self.current_leg.as_mut().unwrap().handle_event(event);
        } else if self.current_activity.is_some() {
            self.current_activity.as_mut().unwrap().handle_event(event);
        } else {
            panic!(
                "Tried to handle an event with neither leg nor activity being initialized! Event type: {}",
                event.type_()
            )
        }

        if let Some(_) = event.as_any().downcast_ref::<PersonArrivalEvent>() {
            self.handle_person_arrival();
        } else if let Some(_) = event.as_any().downcast_ref::<ActivityEndEvent>() {
            self.handle_activity_end();
        }
    }

    pub(crate) fn finish(mut self) -> InternalPlan {
        if let Some(activity) = self.current_activity.take() {
            // if there was an aborted activity before,
            if activity.is_initialized() {
                self.elements.push(Activity(activity.finish()));
            }
        } else if let Some(leg) = self.current_leg.take() {
            // a plan can never end with a leg, so we mark it as aborted and finish it
            self.elements.push(Leg(leg.finish_incomplete()));
        }

        InternalPlan {
            score: None,
            selected: true,
            elements: self.elements,
        }
    }
}

struct PartialActivity {
    pub act_type: Option<Id<String>>,
    pub link_id: Option<Id<Link>>,
    pub coordinate: Option<Coordinate>,
    pub start_time: Option<SimTime>,
    pub end_time: Option<SimTime>,
    pub aborted: bool,
}

impl Default for PartialActivity {
    fn default() -> Self {
        Self {
            act_type: None,
            link_id: None,
            coordinate: None,
            start_time: None,
            end_time: None,
            aborted: false,
        }
    }
}

impl PartialActivity {
    fn is_initialized(&self) -> bool {
        self.act_type.is_some() && self.link_id.is_some()
    }

    fn handle_activity_start(&mut self, event: &ActivityStartEvent) {
        self.act_type = Some(event.act_type.clone());
        self.link_id = Some(event.link.clone());
        self.coordinate = Some(event.coordinate.clone());
        self.start_time = Some(event.time);
    }

    fn handle_activity_end(&mut self, event: &ActivityEndEvent) {
        self.act_type = Some(event.act_type.clone());
        self.link_id = Some(event.link.clone());
        self.coordinate = Some(event.coordinate.clone());
        self.end_time = Some(event.time);
    }

    fn handle_event(&mut self, event: &dyn EventTrait) {
        if let Some(e) = event.as_any().downcast_ref::<ActivityStartEvent>() {
            self.handle_activity_start(e);
        } else if let Some(e) = event.as_any().downcast_ref::<ActivityEndEvent>() {
            self.handle_activity_end(e);
        }
    }

    /// Consuming function turning PartialActivity into an InternalActivity
    fn finish(self) -> InternalActivity {
        let mut activity = InternalActivity::new(
            self.coordinate,
            self.act_type
                .unwrap_or_else(|| panic!("Tried to finish PartialActivity without act type!"))
                .external(),
            self.link_id
                .unwrap_or_else(|| panic!("Tried to finish PartialActivity without link!")),
            self.start_time,
            self.end_time,
            None,
        );
        if self.aborted {
            activity.attributes.insert("aborted", true);
        }
        activity
    }
}

struct PartialLeg {
    pub mode: Option<Id<String>>,
    pub routing_mode: Option<Id<String>>,
    pub dep_time: Option<SimTime>,
    pub trav_time: Option<Duration>,
    pub partial_route: PartialRoute,
}

impl Default for PartialLeg {
    fn default() -> Self {
        Self {
            mode: None,
            routing_mode: None,
            dep_time: None,
            trav_time: None,
            partial_route: PartialRoute::default(),
        }
    }
}

impl PartialLeg {
    fn handle_person_departure(&mut self, event: &PersonDepartureEvent) {
        self.mode = Some(event.leg_mode.clone());
        self.routing_mode = Some(event.routing_mode.clone());
        self.dep_time = Some(event.time);
    }

    fn handle_person_arrival(&mut self, event: &PersonArrivalEvent) {
        self.trav_time = Some(event.time.duration_since(self.dep_time.unwrap()));
    }

    fn handle_event(&mut self, event: &dyn EventTrait) {
        if let Some(e) = event.as_any().downcast_ref::<PersonArrivalEvent>() {
            self.handle_person_arrival(e);
        } else if let Some(e) = event.as_any().downcast_ref::<PersonDepartureEvent>() {
            self.handle_person_departure(e);
        }

        self.partial_route.handle_event(event);
    }

    /// Consuming function turning PartialLeg into an InternalLeg
    fn finish(self) -> InternalLeg {
        InternalLeg::new(
            self.partial_route.finish(),
            self.mode
                .unwrap_or_else(|| panic!("Tried to finish PartialLeg without mode!"))
                .external(),
            self.routing_mode
                .unwrap_or_else(|| panic!("Tried to finish PartialLeg without routing_mode!"))
                .external(),
            self.trav_time
                .unwrap_or_else(|| panic!("Tried to finish PartialLeg without trav_time!")),
            self.dep_time,
        )
    }

    fn finish_incomplete(self) -> InternalLeg {
        let mut leg = InternalLeg {
            mode: self
                .mode
                .unwrap_or_else(|| panic!("Tried to finish PartialLeg without mode!")),
            routing_mode: self.routing_mode,
            dep_time: self.dep_time,
            trav_time: None,
            route: self.partial_route.finish_incomplete(),
            attributes: Default::default(),
        };
        leg.attributes.insert("aborted", true);
        leg
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PartialRouteTypes {
    Generic,
    Network,
    Pt,
}

struct PartialRoute {
    route_type: Option<PartialRouteTypes>,

    // Generic Route Type
    start_link: Option<Id<Link>>,
    end_link: Option<Id<Link>>,
    start_time: Option<SimTime>,
    end_time: Option<SimTime>,
    distance: Option<f64>,
    vehicle: Option<Id<InternalVehicle>>,
    pt_description: Option<InternalPtRouteDescription>,

    //TODO These values are currently unused
    relative_position_on_departure_link: Option<f64>,
    relative_position_on_arrival_link: Option<f64>,

    // Network Route Type
    route: Vec<Id<Link>>, // LinkEnter events contain links entered after the departure link.
}

impl Default for PartialRoute {
    fn default() -> Self {
        Self {
            route_type: None,
            start_link: None,
            end_link: None,
            start_time: None,
            end_time: None,
            distance: None,
            vehicle: None,
            pt_description: None,
            relative_position_on_departure_link: None,
            relative_position_on_arrival_link: None,
            route: Vec::default(),
        }
    }
}

impl PartialRoute {
    fn handle_person_departure(&mut self, event: &PersonDepartureEvent) {
        self.start_time = Some(event.time);
        self.start_link = Some(event.link.clone());
    }

    fn handle_person_arrival(&mut self, event: &PersonArrivalEvent) {
        self.end_time = Some(event.time);
        self.end_link = Some(event.link.clone());
    }

    fn handle_person_enters_vehicle(&mut self, event: &PersonEntersVehicleEvent) {
        if self.route_type == Some(PartialRouteTypes::Generic) {
            panic!("Caught a link enter event on an Generic Route Type!")
        }
        self.route_type = Some(PartialRouteTypes::Network);

        self.vehicle = Some(event.vehicle.clone());
    }

    fn handle_vehicle_enters_traffic(&mut self, event: &VehicleEntersTrafficEvent) {
        self.relative_position_on_departure_link = Some(event.relative_position);
    }

    fn handle_vehicle_leaves_traffic(&mut self, event: &VehicleLeavesTrafficEvent) {
        self.relative_position_on_arrival_link = Some(event.relative_position);
    }

    fn handle_link_enter_event(&mut self, event: &LinkEnterEvent) {
        self.route.push(event.link.clone());
    }

    fn handle_teleportation_arrival(&mut self, event: &TeleportationArrivalEvent) {
        if self.route_type == Some(PartialRouteTypes::Network) {
            panic!("Caught a teleportation event on an Network Route Type!")
        }
        self.route_type = Some(PartialRouteTypes::Generic);

        self.distance = Some(event.distance);
    }

    fn handle_pt_teleportation_arrival(&mut self, event: &PtTeleportationArrivalEvent) {
        if self.route_type == Some(PartialRouteTypes::Network) {
            panic!("Caught a PT teleportation event on a Network Route Type!")
        }
        self.route_type = Some(PartialRouteTypes::Pt);
        self.distance = Some(event.distance);
        self.pt_description = Some(InternalPtRouteDescription {
            transit_route_id: event.route.external().to_string(),
            boarding_time: Some(event.boarding_time),
            transit_line_id: event.line.external().to_string(),
            access_facility_id: event.access_facility.external().to_string(),
            egress_facility_id: event.egress_facility.external().to_string(),
        });
    }

    fn handle_event(&mut self, event: &dyn EventTrait) {
        if let Some(e) = event.as_any().downcast_ref::<PersonDepartureEvent>() {
            self.handle_person_departure(e);
        } else if let Some(e) = event.as_any().downcast_ref::<PersonArrivalEvent>() {
            self.handle_person_arrival(e);
        } else if let Some(e) = event.as_any().downcast_ref::<PersonEntersVehicleEvent>() {
            self.handle_person_enters_vehicle(e);
        } else if let Some(e) = event.as_any().downcast_ref::<VehicleEntersTrafficEvent>() {
            self.handle_vehicle_enters_traffic(e);
        } else if let Some(e) = event.as_any().downcast_ref::<VehicleLeavesTrafficEvent>() {
            self.handle_vehicle_leaves_traffic(e);
        } else if let Some(e) = event.as_any().downcast_ref::<LinkEnterEvent>() {
            self.handle_link_enter_event(e);
        } else if let Some(e) = event.as_any().downcast_ref::<TeleportationArrivalEvent>() {
            self.handle_teleportation_arrival(e);
        } else if let Some(e) = event.as_any().downcast_ref::<PtTeleportationArrivalEvent>() {
            self.handle_pt_teleportation_arrival(e);
        }
    }

    /// Consuming function turning PartialRoute into an InternalRoute
    fn finish(self) -> InternalRoute {
        if matches!(
            self.route_type,
            Some(PartialRouteTypes::Generic | PartialRouteTypes::Pt)
        ) && self.distance.is_none()
        {
            panic!("Tried to finish teleported PartialRoute without distance!");
        }
        if self.route_type == Some(PartialRouteTypes::Network) && self.vehicle.is_none() {
            panic!("Tried to finish NetworkPartialRoute without vehicle!");
        }
        let start_link = self
            .start_link
            .unwrap_or_else(|| panic!("Tried to finish PartialRoute without start_link!"));
        let end_link = self
            .end_link
            .unwrap_or_else(|| panic!("Tried to finish PartialRoute without end_link!"));
        let route_delegate = InternalGenericRoute::new(
            start_link.clone(),
            end_link,
            Some(
                self.end_time
                    .unwrap_or_else(|| panic!("Tried to finish PartialRoute without end_time!"))
                    .duration_since(self.start_time.unwrap_or_else(|| {
                        panic!("Tried to finish PartialRoute without start_time!")
                    })),
            ),
            self.distance,
            self.vehicle,
        );

        match self.route_type {
            Some(PartialRouteTypes::Generic) => InternalRoute::Generic(route_delegate),
            Some(PartialRouteTypes::Network) => {
                let mut links = self.route;
                if links.first() != Some(&start_link) {
                    links.insert(0, start_link.clone());
                }
                if links.last() != Some(route_delegate.end_link()) {
                    links.push(route_delegate.end_link().clone());
                }
                let route = InternalNetworkRoute::new(route_delegate, links);

                InternalRoute::Network(route)
            }
            Some(PartialRouteTypes::Pt) => InternalRoute::Pt(InternalPtRoute {
                generic_delegate: route_delegate,
                description: self.pt_description.unwrap_or_else(|| {
                    panic!("Tried to finish PT PartialRoute without route description!")
                }),
            }),
            None => panic!("Tried to finish a PartialRoute which has no route type!"),
        }
    }

    fn finish_incomplete(self) -> Option<InternalRoute> {
        if self.route_type != Some(PartialRouteTypes::Network) {
            return None;
        }

        let start_link = self
            .start_link
            .unwrap_or_else(|| panic!("Tried to finish PartialRoute without start_link!"));
        let mut links = self.route;
        if links.first() != Some(&start_link) {
            links.insert(0, start_link.clone());
        }
        let current_link = links.last().cloned().unwrap_or_else(|| start_link.clone());
        let delegate =
            InternalGenericRoute::new(start_link, current_link, None, self.distance, self.vehicle);
        Some(InternalRoute::Network(InternalNetworkRoute::new(
            delegate, links,
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::PartialPlan;
    use crate::simulation::events::{
        ActivityEndEventBuilder, LinkEnterEventBuilder, PersonArrivalEventBuilder,
        PersonDepartureEventBuilder, PersonEntersVehicleEventBuilder,
        PtTeleportationArrivalEventBuilder,
    };
    use crate::simulation::id::Id;
    use crate::simulation::scenario::Coordinate;
    use crate::simulation::scenario::population::{InternalPlanElement, InternalRoute};
    use crate::simulation::time::SimTime;
    use macros::deterministic_id_test;

    fn finish_initial_activity(plan: &mut PartialPlan) {
        plan.handle_event(
            &ActivityEndEventBuilder::default()
                .time(SimTime::from_secs(10))
                .person(Id::create("person"))
                .link(Id::create("start"))
                .act_type(Id::create("home"))
                .coordinate(Coordinate::new_2d(1.0, 2.0))
                .build()
                .unwrap(),
        );
    }

    fn depart(plan: &mut PartialPlan, mode: &str) {
        plan.handle_event(
            &PersonDepartureEventBuilder::default()
                .time(SimTime::from_secs(10))
                .person(Id::create("person"))
                .link(Id::create("start"))
                .leg_mode(Id::create(mode))
                .routing_mode(Id::create(mode))
                .build()
                .unwrap(),
        );
    }

    #[deterministic_id_test]
    fn reconstructs_pt_route() {
        let mut partial = PartialPlan::default();
        finish_initial_activity(&mut partial);
        depart(&mut partial, "pt");
        partial.handle_event(
            &PtTeleportationArrivalEventBuilder::default()
                .time(SimTime::from_secs(30))
                .person(Id::create("person"))
                .distance(1_234.0)
                .mode(Id::create("pt"))
                .route(Id::create("route"))
                .line(Id::create("line"))
                .boarding_time(SimTime::from_secs(12))
                .access_facility(Id::create("access"))
                .egress_facility(Id::create("egress"))
                .build()
                .unwrap(),
        );
        partial.handle_event(
            &PersonArrivalEventBuilder::default()
                .time(SimTime::from_secs(30))
                .person(Id::create("person"))
                .link(Id::create("end"))
                .leg_mode(Id::create("pt"))
                .build()
                .unwrap(),
        );

        let plan = partial.finish();
        let InternalPlanElement::Leg(leg) = &plan.elements[1] else {
            panic!("Expected reconstructed leg");
        };
        let Some(InternalRoute::Pt(route)) = &leg.route else {
            panic!("Expected reconstructed PT route");
        };
        assert_eq!(route.start_link().external(), "start");
        assert_eq!(route.end_link().external(), "end");
        assert_eq!(route.generic_delegate().distance(), Some(1_234.0));
        assert_eq!(route.description.transit_route_id, "route");
        assert_eq!(route.description.transit_line_id, "line");
        assert_eq!(
            route.description.boarding_time,
            Some(SimTime::from_secs(12))
        );
        assert_eq!(route.description.access_facility_id, "access");
        assert_eq!(route.description.egress_facility_id, "egress");
    }

    #[deterministic_id_test]
    fn leaves_eventless_plan_empty_and_marks_unfinished_leg_as_aborted() {
        assert!(PartialPlan::default().finish().elements.is_empty());

        let mut leg_plan = PartialPlan::default();
        finish_initial_activity(&mut leg_plan);
        depart(&mut leg_plan, "car");
        leg_plan.handle_event(
            &PersonEntersVehicleEventBuilder::default()
                .time(SimTime::from_secs(10))
                .person(Id::create("person"))
                .vehicle(Id::create("vehicle"))
                .build()
                .unwrap(),
        );
        leg_plan.handle_event(
            &LinkEnterEventBuilder::default()
                .time(SimTime::from_secs(20))
                .link(Id::create("current"))
                .vehicle(Id::create("vehicle"))
                .build()
                .unwrap(),
        );

        let plan = leg_plan.finish();
        let InternalPlanElement::Leg(leg) = &plan.elements[1] else {
            panic!("Expected unfinished leg");
        };
        assert_eq!(leg.attributes.get::<bool>("aborted"), Some(true));
        assert_eq!(leg.trav_time, None);
        let Some(InternalRoute::Network(route)) = &leg.route else {
            panic!("Expected observed network-route prefix");
        };
        assert_eq!(
            route
                .route()
                .iter()
                .map(|link| link.external())
                .collect::<Vec<_>>(),
            vec!["start", "current"]
        );
        assert_eq!(route.generic_delegate().end_link().external(), "current");
    }
}
