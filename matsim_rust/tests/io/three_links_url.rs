use matsim_rust::simulation::config::Config;
use matsim_rust::simulation::scenario::network::Network;
use matsim_rust::simulation::scenario::population::Population;
use matsim_rust::simulation::scenario::vehicles::Garage;
use std::path::PathBuf;

// Pinned to the v0.3.0 tag (before the crate directory was renamed from `rust_qsim` to
// `matsim_rust`) so the remote files stay stable independent of the current branch layout.
const BASE_URL: &str = "https://raw.githubusercontent.com/matsim-vsp/parallel_qsim_rust/refs/tags/v0.3.0/rust_qsim/tests/resources/3-links-url";

#[test]
fn load_files_from_url_have_content() {
    let input_dir = PathBuf::from(BASE_URL);

    // Load network and check if it contains nodes and links
    let net = Network::from_file_as_is(&input_dir.join("3-links-network.xml"));
    assert!(
        !net.nodes().is_empty() && !net.links().is_empty(),
        "Network should contain nodes and links"
    );

    // Load vehicles and check if there is at least one vehicle type
    let mut garage = Garage::from_file(&input_dir.join("vehicles.xml"));
    assert!(
        !garage.vehicle_types.is_empty(),
        "Vehicles file should define at least one vehicle type"
    );

    // Load population and check if there is at least one person
    let pop = Population::from_file(&input_dir.join("1-agent-full-leg.xml"), &mut garage);
    assert!(
        !pop.persons.is_empty(),
        "Population should contain at least one person"
    );

    // Load expected events and check if it's not empty
    let events_url = format!("{}/expected_events.xml", BASE_URL);
    let events = reqwest::blocking::get(&events_url)
        .unwrap_or_else(|error| panic!("Failed to fetch events from {events_url}: {error}"))
        .text()
        .unwrap_or_else(|error| panic!("Failed to read events from {events_url}: {error}"));
    assert!(
        events
            .lines()
            .any(|line| line.trim_start().starts_with("<event ")),
        "Expected events loaded from URL should not be empty"
    );

    // Check if the config can be loaded from URL and matches local config.
    // Note: Only ids.path, output.output_dir, and partitioning.num_parts are compared here.
    let config_url = format!("{}/3-links-config-1.yml", BASE_URL);
    let config_content: Config = Config::from_path(config_url);
    let config_local = "./tests/resources/3-links-url/3-links-config-1.yml";
    let config_content_local: Config = Config::from_path(config_local);

    assert_eq!(
        config_content.ids().path,
        config_content_local.ids().path,
        "Config loaded from URL should match local config"
    );
    assert_eq!(
        config_content.output().output_dir,
        config_content_local.output().output_dir,
        "Config loaded from URL should match local config"
    );
    assert_eq!(
        config_content.partitioning().num_parts,
        config_content_local.partitioning().num_parts,
        "Config loaded from URL should match local config"
    );
}
