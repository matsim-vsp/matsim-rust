use crate::simulation::events::{
    ActivityEndEvent, ActivityStartEvent, EventTrait, LinkEnterEvent, PersonArrivalEvent,
    PersonDepartureEvent, PersonEntersVehicleEvent, TeleportationArrivalEvent,
    VehicleEntersTrafficEvent, VehicleLeavesTrafficEvent,
};
use crate::simulation::id::Id;
use crate::simulation::scenario::Coordinate;
use crate::simulation::scenario::network::Link;
use crate::simulation::scenario::population::InternalPlanElement::{Activity, Leg};
use crate::simulation::scenario::population::{
    InternalActivity, InternalGenericRoute, InternalLeg, InternalNetworkRoute, InternalPlan,
    InternalPlanElement, InternalRoute,
};
use crate::simulation::scenario::vehicles::InternalVehicle;

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

    pub(crate) fn handle_event(&mut self, event: &dyn EventTrait) {
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
        // Check if plan is completely empty, in this case return an empty default plan
        if self.elements.is_empty() {
            return InternalPlan::default();
        }

        // Resolve remaining act
        if !self.current_activity.is_none() {
            // Finish remaining current act
            self.elements
                .push(Activity(self.current_activity.unwrap().finish()));
        }

        InternalPlan {
            selected: true,
            elements: self.elements,
        }
    }
}

struct PartialActivity {
    pub act_type: Option<Id<String>>,
    pub link_id: Option<Id<Link>>,
    pub coordinate: Option<Coordinate>,
    pub start_time: Option<u32>,
    pub end_time: Option<u32>,
    // pub max_dur: Option<u32>, (not meant to be set in the experienced plans)
}

impl Default for PartialActivity {
    fn default() -> Self {
        Self {
            act_type: None,
            link_id: None,
            coordinate: None,
            start_time: None,
            end_time: None,
        }
    }
}

impl PartialActivity {
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
        InternalActivity::new(
            self.coordinate
                .unwrap_or_else(|| panic!("Tried to finish PartialActivity without coordinate!")),
            self.act_type
                .unwrap_or_else(|| panic!("Tried to finish PartialActivity without act type!"))
                .external(),
            self.link_id
                .unwrap_or_else(|| panic!("Tried to finish PartialActivity without link!")),
            self.start_time,
            self.end_time,
            None,
        )
    }
}

struct PartialLeg {
    pub mode: Option<Id<String>>,
    pub routing_mode: Option<Id<String>>,
    pub dep_time: Option<u32>,
    pub trav_time: Option<u32>,
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
        self.trav_time = Some(event.time - self.dep_time.unwrap());
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PartialRouteTypes {
    Generic,
    Network,
}

struct PartialRoute {
    route_type: Option<PartialRouteTypes>,

    // Generic Route Type
    start_link: Option<Id<Link>>,
    end_link: Option<Id<Link>>,
    start_time: Option<u32>,
    end_time: Option<u32>,
    distance: Option<f64>,
    vehicle: Option<Id<InternalVehicle>>,

    //TODO These values are currently unused
    relative_position_on_departure_link: Option<f64>,
    relative_position_on_arrival_link: Option<f64>,

    // Network Route Type
    route: Vec<Id<Link>>, // Currently, route sequence does not contain start or end-link id even though internally
                          // the QSim needs them in the sequence (aleks, May'26)
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
        }
    }

    /// Consuming function turning PartialRoute into an InternalRoute
    fn finish(self) -> InternalRoute {
        if self.route_type == Some(PartialRouteTypes::Generic) && self.distance.is_none() {
            panic!("Tried to finish GenericPartialRoute without distance!");
        }
        if self.route_type == Some(PartialRouteTypes::Network) && self.vehicle.is_none() {
            panic!("Tried to finish NetworkPartialRoute without vehicle!");
        }
        if self.route_type == Some(PartialRouteTypes::Network)
            && self.route.is_empty()
            && (self.start_link != self.end_link)
        {
            // TODO This case seems to happen in simulations sometimes. Check with PH if this is intended.
            // panic!("Tried to finish PartialRoute of type Network with empty vector but differing start and end link!");
        }

        let route_delegate = InternalGenericRoute::new(
            self.start_link
                .unwrap_or_else(|| panic!("Tried to finish PartialRoute without start_link!")),
            self.end_link
                .unwrap_or_else(|| panic!("Tried to finish PartialRoute without end_link!")),
            Some(
                self.end_time
                    .unwrap_or_else(|| panic!("Tried to finish PartialRoute without end_time!"))
                    - self.start_time.unwrap_or_else(|| {
                        panic!("Tried to finish PartialRoute without start_time!")
                    }),
            ),
            self.distance,
            self.vehicle,
        );

        match self.route_type {
            Some(PartialRouteTypes::Generic) => InternalRoute::Generic(route_delegate),
            Some(PartialRouteTypes::Network) => {
                let route = InternalNetworkRoute::new(route_delegate, self.route);

                InternalRoute::Network(route)
            }
            None => panic!("Tried to finish a PartialRoute which has no route type!"),
        }
    }
}
