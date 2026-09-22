use std::sync::Arc;
use clap::Parser;
use tracing::info;
use rust_qsim::simulation::config::{CommandLineArgs, Config};
use rust_qsim::simulation::controller::controller::ControllerBuilder;
use rust_qsim::simulation::id::Id;
use rust_qsim::simulation::logging::init_std_out_logging_thread_local;
use rust_qsim::simulation::scenario::MutableScenario;
use rust_qsim::simulation::scenario::vehicles::InternalVehicleType;

// TODO: This script is not meant to be a library function. It should be outsourced aleks Apr'26

fn main() {
    let _guard = init_std_out_logging_thread_local();

    let args = CommandLineArgs::parse();
    info!("Started with args: {:?}", args);

    // Load and adapt config
    let config = Arc::new(Config::from_args(args));

    // Load and adapt mod
    let mut scenario = MutableScenario::load(config);

    add_teleported_vehicle(&mut scenario, "walk");
    add_teleported_vehicle(&mut scenario, "pt");

    // Create and run simulation
    let controller = ControllerBuilder::default_with_scenario(scenario)
        .build()
        .unwrap();

    controller.run()
}

/// This function was copied from Paul Heinrich's own repository:
/// https://github.com/paulheinr/parallel-qsim-berlin/blob/main/src/main/rust/src/bin/berlin.rs
fn add_teleported_vehicle(scenario: &mut MutableScenario, mode: &str) {
    let id = Id::create(mode);
    scenario.garage.vehicle_types.insert(
        id.clone(),
        InternalVehicleType {
            id,
            length: 1.,
            width: 1.,
            max_v: 1.23,
            pce: 0.1,
            fef: 0.0,
            net_mode: Id::create(mode),
            attributes: Default::default(),
        },
    );

    scenario.population.persons.keys().for_each(|id| {
        scenario.garage.vehicles.insert(
            Id::create(&format!("{}_{}", id, mode)),
            rust_qsim::simulation::scenario::vehicles::InternalVehicle {
                id: Id::create(&format!("{}-{}", id, mode)),
                max_v: 0.833,
                pce: 0.1,
                vehicle_type: Id::create(mode),
                attributes: Default::default(),
            },
        );
    });
}