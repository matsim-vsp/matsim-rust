use macros::deterministic_id_test;
use rust_qsim::simulation::config::{
    CommandLineArgs, CompressionType, Config, StrategySetting, WriteEvents,
};
use rust_qsim::simulation::controller::controller::ControllerBuilder;
use rust_qsim::simulation::events::utils::compare_event_files;
use rust_qsim::simulation::scenario::Scenario;
use std::path::{Path, PathBuf};

#[deterministic_id_test(rust_qsim)]
fn equil_single_part_runs_10_iterations() {
    let config_args = CommandLineArgs::new_with_path("./tests/resources/equil/equil-config-1.yml");
    let mut config = Config::from_args(config_args);
    config.controller_mut().first_iteration = 0;
    config.controller_mut().last_iteration = 9;
    config.output_mut().output_dir =
        PathBuf::from("./test_output/simulation/equil_single_part_10_iterations");

    let scenario = Scenario::load(config);
    let controller = ControllerBuilder::default_with_scenario(scenario)
        .build()
        .unwrap();
    controller.run();
}

#[deterministic_id_test(rust_qsim)]
fn equil_single_part_writes_events_at_interval_and_last_iteration() {
    let output_dir = PathBuf::from("./test_output/simulation/equil_event_interval");
    let mut config = Config::from_args(CommandLineArgs::new_with_path(
        "./tests/resources/equil/equil-config-1.yml",
    ));
    config.controller_mut().first_iteration = 0;
    config.controller_mut().last_iteration = 5;
    config.controller_mut().write_events_interval = 3;
    config.controller_mut().compression_type = CompressionType::Gz;
    config.output_mut().write_events = WriteEvents::File;
    config.output_mut().output_dir = output_dir.clone();

    let scenario = Scenario::load(config);
    let controller = ControllerBuilder::default_with_scenario(scenario)
        .build()
        .unwrap();
    controller.run();

    let iters_dir = output_dir.join("ITERS");
    assert!(
        iters_dir
            .join("it.0")
            .join("events")
            .join("events.0.xml.gz")
            .exists()
    );
    assert!(!iters_dir.join("it.1").join("events").exists());
    assert!(!iters_dir.join("it.2").join("events").exists());
    assert!(
        iters_dir
            .join("it.3")
            .join("events")
            .join("events.0.xml.gz")
            .exists()
    );
    assert!(!iters_dir.join("it.4").join("events").exists());
    assert!(
        iters_dir
            .join("it.5")
            .join("events")
            .join("events.0.xml.gz")
            .exists()
    );
}

#[deterministic_id_test(rust_qsim)]
fn equil_single_part_keep_last_selected_produces_same_events_each_iteration() {
    let output_dir = PathBuf::from("./test_output/simulation/equil_keep_last_selected_same_events");
    let mut config = Config::from_args(CommandLineArgs::new_with_path(
        "./tests/resources/equil/equil-config-1.yml",
    ));
    config.controller_mut().first_iteration = 1;
    config.controller_mut().last_iteration = 3;
    config.controller_mut().write_events_interval = 1;
    config.controller_mut().compression_type = CompressionType::Gz;
    config.output_mut().write_events = WriteEvents::File;
    config.output_mut().output_dir = output_dir.clone();
    config.replanning_mut().strategy_settings = vec![StrategySetting {
        name: "KeepLastSelected".to_string(),
        weight: 1.0,
        subpopulation: "person".to_string(),
    }];

    let scenario = Scenario::load(config);
    let controller = ControllerBuilder::default_with_scenario(scenario)
        .build()
        .unwrap();
    controller.run();

    let iteration_1_events = iteration_events_file(&output_dir, 1);
    assert_events_equal(&iteration_1_events, &iteration_events_file(&output_dir, 2));
    assert_events_equal(&iteration_1_events, &iteration_events_file(&output_dir, 3));
}

#[deterministic_id_test(rust_qsim)]
fn equil_single_part_writes_events_for_single_last_iteration() {
    let output_dir = PathBuf::from("./test_output/simulation/equil_event_interval_single_last");
    let mut config = Config::from_args(CommandLineArgs::new_with_path(
        "./tests/resources/equil/equil-config-1.yml",
    ));
    config.controller_mut().first_iteration = 0;
    config.controller_mut().last_iteration = 0;
    config.controller_mut().write_events_interval = 50;
    config.controller_mut().compression_type = CompressionType::Gz;
    config.output_mut().write_events = WriteEvents::File;
    config.output_mut().output_dir = output_dir.clone();

    let scenario = Scenario::load(config);
    let controller = ControllerBuilder::default_with_scenario(scenario)
        .build()
        .unwrap();
    controller.run();

    assert!(
        output_dir
            .join("ITERS")
            .join("it.0")
            .join("events")
            .join("events.0.xml.gz")
            .exists()
    );
}

fn iteration_events_file(output_dir: &Path, iteration: u32) -> PathBuf {
    output_dir
        .join("ITERS")
        .join(format!("it.{iteration}"))
        .join("events")
        .join("events.0.xml.gz")
}

fn assert_events_equal(left: &Path, right: &Path) {
    compare_event_files(left, right)
        .unwrap_or_else(|error| panic!("Event files differ: {left:?} vs {right:?}: {error}"));
}

#[deterministic_id_test(rust_qsim)]
fn equil_single_part_write_events_none_creates_no_iteration_events() {
    let output_dir = PathBuf::from("./test_output/simulation/equil_event_interval_none");
    let mut config = Config::from_args(CommandLineArgs::new_with_path(
        "./tests/resources/equil/equil-config-1.yml",
    ));
    config.controller_mut().first_iteration = 0;
    config.controller_mut().last_iteration = 3;
    config.controller_mut().write_events_interval = 1;
    config.output_mut().write_events = WriteEvents::None;
    config.output_mut().output_dir = output_dir.clone();

    let scenario = Scenario::load(config);
    let controller = ControllerBuilder::default_with_scenario(scenario)
        .build()
        .unwrap();
    controller.run();

    for iteration in 0..=3 {
        assert!(
            !output_dir
                .join("ITERS")
                .join(format!("it.{iteration}"))
                .join("events")
                .exists()
        );
    }
}

#[deterministic_id_test(rust_qsim)]
fn equil_single_part_writes_plans_at_interval_and_last_iteration() {
    let output_dir = PathBuf::from("./test_output/simulation/equil_plan_interval");
    let mut config = Config::from_args(CommandLineArgs::new_with_path(
        "./tests/resources/equil/equil-config-1.yml",
    ));
    config.controller_mut().first_iteration = 0;
    config.controller_mut().last_iteration = 5;
    config.controller_mut().write_plans_interval = 3;
    config.controller_mut().compression_type = CompressionType::Gz;
    config.output_mut().write_events = WriteEvents::None;
    config.output_mut().output_dir = output_dir.clone();

    let scenario = Scenario::load(config);
    let controller = ControllerBuilder::default_with_scenario(scenario)
        .build()
        .unwrap();
    controller.run();

    let iters_dir = output_dir.join("ITERS");
    assert!(iters_dir.join("it.0").join("output_plans.xml.gz").exists());
    assert!(!iters_dir.join("it.1").join("output_plans.xml.gz").exists());
    assert!(!iters_dir.join("it.2").join("output_plans.xml.gz").exists());
    assert!(iters_dir.join("it.3").join("output_plans.xml.gz").exists());
    assert!(!iters_dir.join("it.4").join("output_plans.xml.gz").exists());
    assert!(iters_dir.join("it.5").join("output_plans.xml.gz").exists());
    assert!(output_dir.join("output_plans.xml.gz").exists());
}

#[deterministic_id_test(rust_qsim)]
fn equil_single_part_writes_plans_for_single_last_iteration() {
    let output_dir = PathBuf::from("./test_output/simulation/equil_plan_interval_single_last");
    let mut config = Config::from_args(CommandLineArgs::new_with_path(
        "./tests/resources/equil/equil-config-1.yml",
    ));
    config.controller_mut().first_iteration = 0;
    config.controller_mut().last_iteration = 0;
    config.controller_mut().write_plans_interval = 50;
    config.controller_mut().compression_type = CompressionType::Gz;
    config.output_mut().write_events = WriteEvents::None;
    config.output_mut().output_dir = output_dir.clone();

    let scenario = Scenario::load(config);
    let controller = ControllerBuilder::default_with_scenario(scenario)
        .build()
        .unwrap();
    controller.run();

    assert!(
        output_dir
            .join("ITERS")
            .join("it.0")
            .join("output_plans.xml.gz")
            .exists()
    );
}
