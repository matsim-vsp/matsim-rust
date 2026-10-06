use std::fs;
use std::path::PathBuf;

use crate::simulation::agents::SimulationAgentLogic;
use crate::simulation::agents::agent::SimulationAgent;
use crate::simulation::id::Id;
use crate::simulation::scenario::Coordinate;
use crate::simulation::scenario::population::{
    InternalActivity, InternalGenericRoute, InternalLeg, InternalNetworkRoute, InternalPerson,
    InternalPlan, InternalRoute,
};
use crate::simulation::scenario::vehicles::InternalVehicleType;
use crate::simulation::time::SimTime;
use crate::simulation::{InternalAttributes, config};
use std::time::Duration;

pub fn create_agent_without_route(id: u64) -> SimulationAgent {
    //inserting a dummy route
    create_agent(id, vec!["0", "1"])
}

pub fn create_agent(id: u64, route: Vec<&str>) -> SimulationAgent {
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

    let mut agent = SimulationAgent::new_plan_based(person);
    agent.advance_plan(SimTime::default());

    agent
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
        capacity: None,
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
        signals: config::SignalFilesConfig::default(),
    }
}

pub fn config() -> config::Config {
    let mut config = config::Config::default();
    config.set_qsim(qsim_config());
    config
}
