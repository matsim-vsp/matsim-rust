use macros::deterministic_id_test;
use rust_qsim::simulation::config::Config;
use rust_qsim::simulation::controller::controller::ControllerBuilder;
use rust_qsim::simulation::events::utils::compare_event_folder;
use rust_qsim::simulation::scenario::Scenario;
use rust_qsim::simulation::scoring::OnlyTravelTimeDependentScoring;
use std::path::PathBuf;

#[deterministic_id_test(rust_qsim)]
fn test_berlin_1() {
    test_berlin(1);
}

#[deterministic_id_test(rust_qsim)]
fn test_berlin_2() {
    test_berlin(2);
}

fn test_berlin(parts: u32) {
    let mut config = Config::from_path("./assets/berlin-v6.4/config.yml");
    config.partitioning_mut().num_parts = parts;
    let output_dir = PathBuf::from(format!(
        "./test_output/simulation/berlin-v6.4-0.1pct-{}",
        parts
    ));
    config.output_mut().output_dir = output_dir.clone();

    let scenario = Scenario::load(config);
    let controller = ControllerBuilder::default_with_scenario(scenario)
        .scoring_function(Box::new(OnlyTravelTimeDependentScoring))
        .build()
        .unwrap();
    controller.run();

    compare_event_folder(
        "./tests/resources/berlin-v6.4-0.1pct",
        output_dir.join("events"),
    )
    .unwrap();
}
