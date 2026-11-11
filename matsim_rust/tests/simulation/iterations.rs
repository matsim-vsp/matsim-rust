use macros::deterministic_id_test;
use matsim_rust::simulation::config::{
    CommandLineArgs, CompressionType, Config, StrategySetting, WriteEvents,
};
use matsim_rust::simulation::controller::controller::ControllerBuilder;
use matsim_rust::simulation::events::utils::compare_event_files;
use matsim_rust::simulation::scenario::Scenario;
use matsim_rust::simulation::scenario::population::Population;
use matsim_rust::simulation::scenario::vehicles::Garage;
use std::path::{Path, PathBuf};

#[deterministic_id_test(matsim_rust)]
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

#[deterministic_id_test(matsim_rust)]
fn equil_single_part_writes_events_at_interval_and_last_iteration() {
    let output_dir = PathBuf::from("./test_output/simulation/equil_event_interval");
    let mut config = Config::from_args(CommandLineArgs::new_with_path(
        "./tests/resources/equil/equil-config-1.yml",
    ));
    config.controller_mut().first_iteration = 0;
    config.controller_mut().last_iteration = 5;
    config.controller_mut().write_events_interval = 3;
    config.output_mut().compression_type = CompressionType::Gz;
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

#[deterministic_id_test(matsim_rust)]
fn equil_single_part_keep_last_selected_produces_same_events_each_iteration() {
    let output_dir = PathBuf::from("./test_output/simulation/equil_keep_last_selected_same_events");
    let mut config = Config::from_args(CommandLineArgs::new_with_path(
        "./tests/resources/equil/equil-config-1.yml",
    ));
    config.controller_mut().first_iteration = 1;
    config.controller_mut().last_iteration = 3;
    config.controller_mut().write_events_interval = 1;
    config.output_mut().compression_type = CompressionType::Gz;
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

#[deterministic_id_test(matsim_rust)]
fn equil_single_part_writes_events_for_single_last_iteration() {
    let output_dir = PathBuf::from("./test_output/simulation/equil_event_interval_single_last");
    let mut config = Config::from_args(CommandLineArgs::new_with_path(
        "./tests/resources/equil/equil-config-1.yml",
    ));
    config.controller_mut().first_iteration = 0;
    config.controller_mut().last_iteration = 0;
    config.controller_mut().write_events_interval = 50;
    config.output_mut().compression_type = CompressionType::Gz;
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

#[deterministic_id_test(matsim_rust)]
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

#[deterministic_id_test(matsim_rust)]
fn equil_single_part_writes_plans_at_interval_and_last_iteration() {
    let output_dir = PathBuf::from("./test_output/simulation/equil_plan_interval");
    let mut config = Config::from_args(CommandLineArgs::new_with_path(
        "./tests/resources/equil/equil-config-1.yml",
    ));
    config.controller_mut().first_iteration = 0;
    config.controller_mut().last_iteration = 5;
    config.controller_mut().write_plans_interval = 3;
    config.output_mut().compression_type = CompressionType::Gz;
    config.output_mut().write_events = WriteEvents::None;
    config.output_mut().output_dir = output_dir.clone();

    let scenario = Scenario::load(config);
    let controller = ControllerBuilder::default_with_scenario(scenario)
        .build()
        .unwrap();
    controller.run();

    let iters_dir = output_dir.join("ITERS");
    for iteration in 0..=5 {
        for stem in ["plans", "experienced_plans"] {
            let path = iters_dir
                .join(format!("it.{iteration}"))
                .join(format!("{iteration}.{stem}.xml.gz"));
            assert_eq!(
                matches!(iteration, 0 | 3 | 5),
                path.exists(),
                "Unexpected output state for {}",
                path.display()
            );
        }
    }
    assert!(output_dir.join("output_plans.xml.gz").exists());
    assert!(output_dir.join("output_experienced_plans.xml.gz").exists());
}

#[deterministic_id_test(matsim_rust)]
fn equil_single_part_writes_plans_for_single_last_iteration() {
    let output_dir = PathBuf::from("./test_output/simulation/equil_plan_interval_single_last");
    let mut config = Config::from_args(CommandLineArgs::new_with_path(
        "./tests/resources/equil/equil-config-1.yml",
    ));
    config.controller_mut().first_iteration = 0;
    config.controller_mut().last_iteration = 0;
    config.controller_mut().write_plans_interval = 50;
    config.output_mut().compression_type = CompressionType::Gz;
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
            .join("0.plans.xml.gz")
            .exists()
    );
    assert!(
        output_dir
            .join("ITERS")
            .join("it.0")
            .join("0.experienced_plans.xml.gz")
            .exists()
    );
    assert!(output_dir.join("output_plans.xml.gz").exists());
    assert!(output_dir.join("output_experienced_plans.xml.gz").exists());
}

#[deterministic_id_test(matsim_rust)]
fn controller_writes_prefixed_output_files_and_preserves_event_names() {
    for (compression, extension) in [
        (CompressionType::Gz, "xml.gz"),
        (CompressionType::Proto, "binpb"),
    ] {
        let output_dir = PathBuf::from(format!(
            "./test_output/simulation/output_run_id_{extension}"
        ));
        let mut config = Config::from_args(CommandLineArgs::new_with_path(
            "./tests/resources/pt_tutorial/pt_tutorial_config.yml",
        ));
        config.output_mut().output_dir = output_dir.clone();
        config.output_mut().run_id = Some("demo".to_string());
        config.output_mut().compression_type = compression;
        config.output_mut().write_events = WriteEvents::File;
        config.controller_mut().first_iteration = 2;
        config.controller_mut().last_iteration = 4;
        config.controller_mut().write_plans_interval = 3;
        config.controller_mut().write_events_interval = 3;

        ControllerBuilder::default_with_scenario(Scenario::load(config))
            .build()
            .unwrap()
            .run();

        for (stem, extension) in [
            ("config", "yml"),
            ("scoring_parameters", "yml"),
            ("network", extension),
            ("facilities", extension),
            ("vehicles", extension),
            ("transitSchedule", extension),
            ("transitVehicles", extension),
            ("plans", extension),
            ("experienced_plans", extension),
        ] {
            let path = output_dir.join(format!("demo.output_{stem}.{extension}"));
            assert!(path.exists(), "Missing {}", path.display());
            assert!(
                !output_dir
                    .join(format!("output_{stem}.{extension}"))
                    .exists()
            );
        }
        assert_eq!(
            compression == CompressionType::Proto,
            output_dir.join("demo.output_ids.binpb").exists()
        );
        assert!(!output_dir.join("output_ids.binpb").exists());
        let restored = Config::from_path(output_dir.join("demo.output_config.yml"));
        assert_eq!(Some("demo"), restored.output().run_id.as_deref());

        for iteration in 2..=4 {
            let iter_path = output_dir.join("ITERS").join(format!("it.{iteration}"));
            for stem in ["plans", "experienced_plans"] {
                let path = iter_path.join(format!("demo.{iteration}.{stem}.{extension}"));
                assert_eq!(
                    iteration != 2,
                    path.exists(),
                    "Unexpected output state for {}",
                    path.display()
                );
                assert!(
                    !iter_path
                        .join(format!("output_{stem}.{extension}"))
                        .exists()
                );
            }
            assert_eq!(
                iteration != 2,
                iter_path
                    .join("events")
                    .join(format!("events.0.{extension}"))
                    .exists()
            );
        }

        for stem in ["plans", "experienced_plans"] {
            let root_file = output_dir.join(format!("demo.output_{stem}.{extension}"));
            let iteration_file = output_dir
                .join("ITERS")
                .join("it.4")
                .join(format!("demo.4.{stem}.{extension}"));
            let root_population = Population::from_file(&root_file, &mut Garage::new());
            let iteration_population = Population::from_file(&iteration_file, &mut Garage::new());
            assert!(!root_population.persons.is_empty());
            assert_eq!(root_population, iteration_population);
        }
        let events_name = format!("events.0.{extension}");
        assert_eq!(
            std::fs::read(
                output_dir
                    .join("ITERS")
                    .join("it.4")
                    .join("events")
                    .join(&events_name)
            )
            .unwrap(),
            std::fs::read(output_dir.join("events").join(&events_name)).unwrap()
        );
    }
}
