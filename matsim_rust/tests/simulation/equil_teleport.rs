use macros::deterministic_id_test;
use matsim_rust::simulation::config::{CommandLineArgs, Config};
use matsim_rust::simulation::controller::controller::ControllerBuilder;
use matsim_rust::simulation::events::utils::compare_event_folder;
use matsim_rust::simulation::scenario::Scenario;

// one agent having a network route, car being not a main mode => simulation should teleport the agent
#[deterministic_id_test(matsim_rust)]
fn teleport_network_route() {
    let config_args = CommandLineArgs::new_with_path(
        "./tests/resources/equil/equil-config-teleport-network-route.yml",
    );

    let config = Config::from_args(config_args);
    let output_dir = config.output().output_dir.clone();
    let scenario = Scenario::load(config);
    let controller = ControllerBuilder::default_with_scenario(scenario)
        .build()
        .unwrap();
    controller.run();
    compare_event_folder(
        "./tests/resources/equil/expected_events_teleport_network_route",
        output_dir.join("events"),
    )
    .unwrap();
}

// one agent having a generic route, car being not a main mode => simulation should teleport the agent
#[deterministic_id_test(matsim_rust)]
fn teleport_generic_route() {
    let config_args = CommandLineArgs::new_with_path(
        "./tests/resources/equil/equil-config-teleport-generic-route.yml",
    );

    let config = Config::from_args(config_args);
    let output_dir = config.output().output_dir.clone();
    let scenario = Scenario::load(config);
    let controller = ControllerBuilder::default_with_scenario(scenario)
        .build()
        .unwrap();
    controller.run();
    compare_event_folder(
        "./tests/resources/equil/expected_events_teleport_generic_route",
        output_dir.join("events"),
    )
    .unwrap();
}

// one agent having a network route, car being a main mode => already implemented
#[deterministic_id_test(matsim_rust)]
fn simulate_network_route() {
    let config_args = CommandLineArgs::new_with_path(
        "./tests/resources/equil/equil-config-simulate-network-route.yml",
    );

    let config = Config::from_args(config_args);
    let output_dir = config.output().output_dir.clone();
    let scenario = Scenario::load(config);
    let controller = ControllerBuilder::default_with_scenario(scenario)
        .build()
        .unwrap();
    controller.run();
    compare_event_folder(
        "./tests/resources/equil/expected_events_simulate_network_route",
        output_dir.join("events"),
    )
    .unwrap();
}

// one agent having a generic route, car being a main mode => simulation should crash
#[deterministic_id_test(matsim_rust)]
#[should_panic]
fn simulate_generic_route_panics() {
    let config_args = CommandLineArgs::new_with_path(
        "./tests/resources/equil/equil-config-simulate-generic-route-panics.yml",
    );

    let scenario = Scenario::load(Config::from_args(config_args));
    let controller = ControllerBuilder::default_with_scenario(scenario)
        .build()
        .unwrap();
    controller.run();
}
