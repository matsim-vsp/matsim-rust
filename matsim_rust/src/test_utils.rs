use std::fs;
use std::path::PathBuf;

use crate::simulation::agents::agent_logic::PlanBasedSimulationLogic;
use crate::simulation::agents::{NetworkDriverAgent, PassengerAgent};
use crate::simulation::id::Id;
use crate::simulation::scenario::Coordinate;
use crate::simulation::scenario::population::{
    InternalActivity, InternalGenericRoute, InternalLeg, InternalNetworkRoute, InternalPerson,
    InternalPlan, InternalPtRoute, InternalPtRouteDescription, InternalRoute,
};
use crate::simulation::scenario::vehicles::InternalVehicleType;
use crate::simulation::time::SimTime;
use crate::simulation::{InternalAttributes, config};
use std::time::Duration;

pub fn create_agent_without_route(id: u64) -> NetworkDriverAgent {
    //inserting a dummy route
    create_agent(id, vec!["0", "1"])
}

pub fn create_agent(id: u64, route: Vec<&str>) -> NetworkDriverAgent {
    let generic_route = InternalGenericRoute::new(
        Id::create(route.first().unwrap()),
        Id::create(route.last().unwrap()),
        None,
        None,
        None,
    );

    let vec = route.into_iter().map(Id::create).collect();

    let net_route = InternalNetworkRoute::new(generic_route, vec);

    let leg = InternalLeg::new(
        InternalRoute::Network(net_route),
        "car",
        "car",
        Duration::default(),
        None,
    );
    let act = InternalActivity::new(
        Some(Coordinate::default()),
        "act",
        Id::create("1"),
        None,
        None,
        None,
    );
    let mut plan = InternalPlan::default();
    plan.add_act(act);
    plan.add_leg(leg);
    let person = InternalPerson::new(Id::create(id.to_string().as_str()), plan);

    let agent = PlanBasedSimulationLogic::new_agent(person)
        .finish_activity(SimTime::default())
        .expect_leg();

    agent.try_into_network().unwrap()
}

/// An agent on a transit leg with `line` from stop `access` on link `start_link` to stop `egress` on
/// link `end_link`.
pub fn create_pt_agent(
    id: u64,
    line: &str,
    access: &str,
    egress: &str,
    start_link: &str,
    end_link: &str,
) -> PassengerAgent {
    let route = InternalPtRoute {
        generic_delegate: InternalGenericRoute::new(
            Id::create(start_link),
            Id::create(end_link),
            None,
            None,
            None,
        ),
        description: InternalPtRouteDescription {
            // Passengers board any route of their line.
            transit_route_id: Id::create(""),
            boarding_time: None,
            transit_line_id: Id::create(line),
            access_facility_id: Id::create(access),
            egress_facility_id: Id::create(egress),
        },
    };
    let activity = |link: &str| {
        InternalActivity::new(
            Some(Coordinate::default()),
            "act",
            Id::create(link),
            None,
            None,
            None,
        )
    };
    let mut plan = InternalPlan::default();
    plan.add_act(activity(start_link));
    plan.add_leg(InternalLeg::new(
        InternalRoute::Pt(route),
        "pt",
        "pt",
        Duration::default(),
        None,
    ));
    plan.add_act(activity(end_link));
    let person = InternalPerson::new(Id::create(id.to_string().as_str()), plan);

    PlanBasedSimulationLogic::new_agent(person)
        .finish_activity(SimTime::default())
        .expect_leg()
        .try_into_passenger()
        .unwrap()
}

pub fn create_folders(path: PathBuf) -> PathBuf {
    fs::create_dir_all(&path)
        .unwrap_or_else(|_e| panic!("Failed to create folders for path {path:?}"));
    path
}

pub fn create_vehicle_type(
    id: &Id<InternalVehicleType>,
    net_mode: Id<String>,
) -> InternalVehicleType {
    InternalVehicleType {
        id: id.clone(),
        length: 0.0,
        width: 0.0,
        max_v: 0.0,
        pce: 0.0,
        fef: 0.0,
        net_mode,
        attributes: InternalAttributes::default(),
    }
}

pub fn qsim_config() -> config::QSim {
    config::QSim {
        start_time: 0,
        end_time: 0,
        ticks_per_second: 1,
        sample_size: 1.0,
        stuck_threshold: u32::MAX,
        main_modes: vec![String::from("car")],
        vehicles_source: config::VehiclesSource::default(),
        traffic_dynamics: config::TrafficDynamics::Queue,
        link_dynamics: config::LinkDynamics::Fifo,
    }
}

pub fn config() -> config::Config {
    let mut config = config::Config::default();
    config.set_qsim(qsim_config());
    config
}
