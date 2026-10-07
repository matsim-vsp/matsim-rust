use macros::deterministic_id_test;
use matsim_rust::simulation::config::{Config, OverwriteFiles};
use matsim_rust::simulation::controller::controller::ControllerBuilder;
use matsim_rust::simulation::scenario::Scenario;
use std::path::PathBuf;

/// Test that with default structs the simulation runs without errors
#[deterministic_id_test(matsim_rust)]
fn empty_simulation_runs() {
    let mut config = Config::default();
    config.controller_mut().last_iteration = 0;
    config.output_mut().overwrite_files = OverwriteFiles::DeleteDirectoryIfExists;
    config.output_mut().output_dir = "./test_output/simulation/empty".parse().unwrap();
    let scenario = Scenario::load(config);
    let controller = ControllerBuilder::default_with_scenario(scenario)
        .build()
        .unwrap();
    controller.run();
}

/// Test that the configured controller iteration bounds are inclusive and do not
/// implicitly start at zero.
#[deterministic_id_test(matsim_rust)]
fn empty_simulation_uses_configured_iteration_range() {
    let output_dir = PathBuf::from("./test_output/simulation/empty_iteration_range");
    let mut config = Config::default();
    config.controller_mut().first_iteration = 2;
    config.controller_mut().last_iteration = 3;
    config.qsim_mut().end_time = 0;
    config.controller_mut().write_plans_interval = 1;
    config.output_mut().overwrite_files = OverwriteFiles::DeleteDirectoryIfExists;
    config.output_mut().output_dir = output_dir.clone();

    let scenario = Scenario::load(config);
    let controller = ControllerBuilder::default_with_scenario(scenario)
        .build()
        .unwrap();
    controller.run();

    let iters_dir = output_dir.join("ITERS");
    assert!(!iters_dir.join("it.0").exists());
    assert!(!iters_dir.join("it.1").exists());
    assert!(iters_dir.join("it.2").exists());
    assert!(iters_dir.join("it.3").exists());
}
