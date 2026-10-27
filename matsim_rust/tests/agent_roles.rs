use macros::deterministic_id_test;
use matsim_rust::simulation::agents::agent::SimulationAgent;
use matsim_rust::simulation::agents::{NetworkDriverCommands, NetworkDriverLogic};
use matsim_rust::simulation::id::Id;
use matsim_rust::simulation::scenario::Coordinate;
use matsim_rust::simulation::scenario::network::Link;
use matsim_rust::simulation::scenario::population::{
    InternalActivity, InternalGenericRoute, InternalLeg, InternalNetworkRoute, InternalPerson,
    InternalPlan, InternalRoute,
};
use matsim_rust::simulation::scenario::vehicles::InternalVehicle;
use matsim_rust::simulation::time::SimTime;
use matsim_rust::simulation::vehicles::SimulationVehicle;
use std::time::Duration;

// No SimulationAgentLogic implementation, person, or activity plan is needed to drive.
struct Driver {
    links: Vec<Id<Link>>,
    position: usize,
    mode: Id<String>,
}

impl NetworkDriverLogic for Driver {
    fn curr_link_id(&self) -> Option<&Id<Link>> {
        self.links.get(self.position)
    }
    fn peek_next_link_id(&self) -> Option<&Id<Link>> {
        self.links.get(self.position + 1)
    }
    fn mode(&self) -> &Id<String> {
        &self.mode
    }
    fn is_wanting_to_arrive_on_current_link(&self) -> bool {
        // A custom driver may want to stop before exhausting its route.
        true
    }
}

impl NetworkDriverCommands for Driver {
    fn leave_link(&mut self, _now: SimTime) {
        self.position += 1;
    }
}

#[deterministic_id_test(matsim_rust)]
fn independent_driver_role_preserves_custom_arrival_decisions() {
    let mut driver = Driver {
        links: vec![Id::create("a"), Id::create("b")],
        position: 0,
        mode: Id::create("car"),
    };
    {
        let view: &dyn NetworkDriverLogic = &driver;
        assert_eq!(view.curr_link_id().unwrap().external(), "a");
        assert_eq!(view.peek_next_link_id().unwrap().external(), "b");
        assert_eq!(view.mode().external(), "car");
        assert!(view.is_wanting_to_arrive_on_current_link());
    }
    driver.leave_link(SimTime::default());
    assert_eq!(driver.curr_link_id().unwrap().external(), "b");
    let view: &dyn NetworkDriverLogic = &driver;
    assert!(view.peek_next_link_id().is_none());
    assert!(view.is_wanting_to_arrive_on_current_link());
}

#[deterministic_id_test(matsim_rust)]
fn vehicle_movement_still_advances_passengers_with_generic_routes() {
    let driver = network_agent(1, vec!["a", "b", "c"]);
    let mut person = network_agent(2, vec!["a", "c"]).into_person();
    person.selected_plan_mut().elements[1]
        .as_leg_mut()
        .unwrap()
        .route = Some(InternalRoute::Generic(InternalGenericRoute::new(
        Id::get_from_ext("a"),
        Id::get_from_ext("c"),
        None,
        None,
        None,
    )));
    let mut passenger = SimulationAgent::new_plan_based(person);
    passenger.activity_mut().start_next_leg(SimTime::default());
    let mut vehicle = SimulationVehicle::new(
        InternalVehicle {
            id: Id::create("vehicle"),
            vehicle_type: Id::create("car"),
            max_v: 100.0,
            pce: 1.0,
            attributes: Default::default(),
        },
        Some(driver),
        vec![passenger],
    );
    vehicle.leave_link(SimTime::default());
    assert_eq!(
        vehicle
            .driver()
            .network_driver()
            .curr_link_id()
            .unwrap()
            .external(),
        "b"
    );
    assert_eq!(
        vehicle.passengers()[0]
            .leg()
            .curr_link_id()
            .unwrap()
            .external(),
        "c"
    );
}

#[deterministic_id_test(matsim_rust)]
fn materialization_preserves_unfinished_routes_and_all_plans() {
    for adaptive in [false, true] {
        let mut person = network_person(1, vec!["a", "b", "c"]);
        person.plans_mut()[0].score = Some(42.0);
        let mut unselected = person.plans()[0].clone();
        unselected.selected = false;
        unselected.score = Some(-1.0);
        unselected.elements[1].as_leg_mut().unwrap().route = None;
        person.plans_mut().insert(0, unselected);
        let expected = person.clone();
        let mut agent = if adaptive {
            SimulationAgent::new_adaptive_plan_based(person)
        } else {
            SimulationAgent::new_plan_based(person)
        };
        agent.activity_mut().start_next_leg(SimTime::from_secs(5));
        agent.network_driver_mut().leave_link(SimTime::from_secs(6));
        assert_eq!(
            agent.network_driver().curr_link_id().unwrap().external(),
            "b"
        );
        assert_eq!(
            agent
                .network_driver()
                .peek_next_link_id()
                .unwrap()
                .external(),
            "c"
        );
        assert_eq!(agent.into_person(), expected);
    }
}

fn network_person(id: u64, links: Vec<&str>) -> InternalPerson {
    let generic = InternalGenericRoute::new(
        Id::create(links[0]),
        Id::create(links.last().unwrap()),
        None,
        None,
        None,
    );
    let mut plan = InternalPlan::default();
    plan.add_act(InternalActivity::new(
        Some(Coordinate::default()),
        "home",
        Id::create(links[0]),
        None,
        None,
        None,
    ));
    plan.add_leg(InternalLeg::new(
        InternalRoute::Network(InternalNetworkRoute::new(
            generic,
            links.into_iter().map(Id::create).collect(),
        )),
        "car",
        "car",
        Duration::ZERO,
        None,
    ));
    InternalPerson::new(Id::create(&id.to_string()), plan)
}

fn network_agent(id: u64, links: Vec<&str>) -> SimulationAgent {
    let mut agent = SimulationAgent::new_plan_based(network_person(id, links));
    agent.activity_mut().start_next_leg(SimTime::default());
    agent
}
