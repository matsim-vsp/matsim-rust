//! The route service that SILO queries after QSim has finished.
//!
//! SILO resolves origin and destination to network links itself, so a request may name
//! links or persons this population never loaded. Those are ordinary bad requests and
//! must not take the service down with them.

use macros::deterministic_id_test;
use matsim_rust::external_services::silo_routing;
use matsim_rust::simulation::config::{CommandLineArgs, Config};
use matsim_rust::simulation::controller::controller::ControllerBuilder;
use matsim_rust::simulation::events::utils::compare_event_folder;
use matsim_rust::simulation::id::Id;
use matsim_rust::simulation::scenario::Scenario;
use matsim_rust::simulation::scenario::network::{Network, Node};
use nohash_hasher::IntMap;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::Path;
use std::time::{Duration, Instant};

const START: (f64, f64) = (-20000.0, 0.0);
const HOME_LINK: &str = "1";
const WORK_LINK: &str = "20";
const WORK_COORD: (f64, f64) = (0.0, 0.0);

struct RouteClient {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
}

impl RouteClient {
    fn request(&mut self, request: &str) -> Value {
        self.writer
            .write_all(format!("{request}\n").as_bytes())
            .unwrap();
        self.writer.flush().unwrap();
        let mut response = String::new();
        self.reader.read_line(&mut response).unwrap();
        assert!(
            !response.is_empty(),
            "route service closed the connection instead of answering"
        );
        serde_json::from_str(&response).unwrap()
    }
}

/// Runs QSim on the equil example and then serves its router, the way `local_qsim` does
/// when SILO passes `--routing-service-ready-file`.
fn start_route_service(ready_file: &Path) -> RouteClient {
    let config = Config::from_args(CommandLineArgs::new_with_path(
        "./tests/resources/equil/equil-config-silo-routing.yml",
    ));
    let (router, population) = ControllerBuilder::default_with_scenario(Scenario::load(config))
        .build()
        .unwrap()
        .run();
    let owned_ready_file = ready_file.to_path_buf();
    std::thread::spawn(move || {
        silo_routing::serve(router, population, "127.0.0.1:0", &owned_ready_file).unwrap();
    });

    let deadline = Instant::now() + Duration::from_secs(30);
    while !ready_file.exists() {
        assert!(Instant::now() < deadline, "route service never started");
        std::thread::sleep(Duration::from_millis(20));
    }
    let address = std::fs::read_to_string(ready_file).unwrap();
    let stream = TcpStream::connect(address.trim()).unwrap();
    RouteClient {
        reader: BufReader::new(stream.try_clone().unwrap()),
        writer: stream,
    }
}

fn start_route_service_with_network(
    ready_file: &Path,
    network_path: &Path,
    worker_count: u32,
    output_dir: &Path,
    diagnostics_path: Option<&Path>,
) -> (RouteClient, Vec<u8>) {
    let config_source =
        std::fs::read_to_string("./tests/resources/equil/equil-config-silo-routing.yml")
            .unwrap()
            .replace(
                "method: !Metis\n      vertex_weight:\n        - Constant",
                "method: None",
            );
    std::fs::create_dir_all(output_dir).unwrap();
    let config_path = output_dir.join("config.yml");
    std::fs::write(&config_path, config_source).unwrap();
    let overrides = vec![
        (
            "network.path".to_string(),
            network_path.display().to_string(),
        ),
        (
            "population.path".to_string(),
            std::fs::canonicalize("./assets/equil/equil-1-plan.xml")
                .unwrap()
                .display()
                .to_string(),
        ),
        (
            "vehicles.path".to_string(),
            std::fs::canonicalize("./assets/equil/equil-vehicles.xml")
                .unwrap()
                .display()
                .to_string(),
        ),
        (
            "partitioning.num_parts".to_string(),
            worker_count.to_string(),
        ),
        (
            "output.output_dir".to_string(),
            output_dir.display().to_string(),
        ),
    ];
    let config = Config::from_args(CommandLineArgs {
        config: config_path.display().to_string(),
        overrides,
    });
    let mut controller = ControllerBuilder::default_with_scenario(Scenario::load(config));
    if let Some(path) = diagnostics_path {
        controller = controller.partition_diagnostics_output(path);
    }
    let (router, population) = controller.build().unwrap().run();
    assert_eq!(1, population.persons.len());
    let final_plans = std::fs::read(output_dir.join("output_plans.xml.zst")).unwrap();
    let owned_ready_file = ready_file.to_path_buf();
    std::thread::spawn(move || {
        silo_routing::serve(router, population, "127.0.0.1:0", &owned_ready_file).unwrap();
    });

    let deadline = Instant::now() + Duration::from_secs(30);
    while !ready_file.exists() {
        assert!(Instant::now() < deadline, "route service never started");
        std::thread::sleep(Duration::from_millis(20));
    }
    let address = std::fs::read_to_string(ready_file).unwrap();
    let stream = TcpStream::connect(address.trim()).unwrap();
    (
        RouteClient {
            reader: BufReader::new(stream.try_clone().unwrap()),
            writer: stream,
        },
        final_plans,
    )
}

fn start_pt_route_service(
    ready_file: &Path,
    config_path: &Path,
    population_path: &Path,
    network_path: &Path,
    worker_count: u32,
    output_dir: &Path,
) -> RouteClient {
    let config = Config::from_args(CommandLineArgs {
        config: config_path.display().to_string(),
        overrides: vec![
            (
                "population.path".to_owned(),
                population_path.display().to_string(),
            ),
            (
                "network.path".to_owned(),
                network_path.display().to_string(),
            ),
            (
                "partitioning.num_parts".to_owned(),
                worker_count.to_string(),
            ),
            (
                "output.output_dir".to_owned(),
                output_dir.display().to_string(),
            ),
        ],
    });
    let (router, population) = ControllerBuilder::default_with_scenario(Scenario::load(config))
        .build()
        .unwrap()
        .run();
    let owned_ready_file = ready_file.to_path_buf();
    std::thread::spawn(move || {
        silo_routing::serve(router, population, "127.0.0.1:0", &owned_ready_file).unwrap();
    });

    let deadline = Instant::now() + Duration::from_secs(30);
    while !ready_file.exists() {
        assert!(Instant::now() < deadline, "route service never started");
        std::thread::sleep(Duration::from_millis(20));
    }
    let address = std::fs::read_to_string(ready_file).unwrap();
    let stream = TcpStream::connect(address.trim()).unwrap();
    RouteClient {
        reader: BufReader::new(stream.try_clone().unwrap()),
        writer: stream,
    }
}

fn car_request(from_link: &str, to_link: &str, from: (f64, f64), to: (f64, f64)) -> String {
    json!({
        "mode": "car",
        "from_x": from.0,
        "from_y": from.1,
        "from_link_id": from_link,
        "to_x": to.0,
        "to_y": to.1,
        "to_link_id": to_link,
        "departure_time_seconds": 6.0 * 3600.0,
        "person_id": Value::Null,
    })
    .to_string()
}

fn error_of(response: &Value) -> &str {
    assert_eq!(response["travel_time_seconds"], Value::Null);
    assert_eq!(response["distance_meters"], Value::Null);
    response["error"]
        .as_str()
        .expect("response without an error")
}

fn category_of(response: &Value) -> &str {
    response["failure_category"]
        .as_str()
        .expect("response without a failure category")
}

#[deterministic_id_test(matsim_rust)]
fn routes_a_car_leg_between_two_links() {
    let directory = tempfile::tempdir().unwrap();
    let mut client = start_route_service(&directory.path().join("routing-service.address"));

    let request = car_request(HOME_LINK, WORK_LINK, START, WORK_COORD);
    let first = client.request(&request);
    assert_eq!(first["error"], Value::Null);
    assert_eq!(first["failure_category"], Value::Null);
    assert_eq!(first["distance_meters"], 25000.0);
    assert!(first["travel_time_seconds"].as_f64().unwrap() > 0.0);

    // SILO repeats the same origin/destination pair for every agent, so the answer has to
    // be reproducible.
    assert_eq!(client.request(&request), first);
}

#[deterministic_id_test(matsim_rust)]
fn same_link_request_is_a_zero_length_route() {
    let directory = tempfile::tempdir().unwrap();
    let mut client = start_route_service(&directory.path().join("routing-service.address"));

    // MATSim answers a request that starts and ends on one link with a zero-length route
    // instead of driving a loop back onto the link. This is SILO's intrazonal case.
    let response = client.request(&car_request(HOME_LINK, HOME_LINK, START, START));
    assert_eq!(response["error"], Value::Null);
    assert_eq!(response["travel_time_seconds"], 0.0);
    assert_eq!(response["distance_meters"], 0.0);
}

#[deterministic_id_test(matsim_rust)]
fn same_link_pt_request_is_handled_by_the_pt_router() {
    let directory = tempfile::tempdir().unwrap();
    let mut client = start_route_service(&directory.path().join("routing-service.address"));

    let mut request: Value =
        serde_json::from_str(&car_request(HOME_LINK, HOME_LINK, START, START)).unwrap();
    request["mode"] = json!("pt");
    let response = client.request(&request.to_string());
    assert!(error_of(&response).contains("pt"));
}

#[deterministic_id_test(matsim_rust)]
fn unknown_link_is_reported_and_the_connection_stays_open() {
    let directory = tempfile::tempdir().unwrap();
    let mut client = start_route_service(&directory.path().join("routing-service.address"));

    let response = client.request(&car_request("not-a-link", WORK_LINK, START, WORK_COORD));
    assert!(error_of(&response).contains("not-a-link"));
    assert_eq!(category_of(&response), "invalid_link");

    // A rejected request must not cost SILO its pooled connection.
    assert_eq!(
        client.request(&car_request(HOME_LINK, WORK_LINK, START, WORK_COORD))["error"],
        Value::Null
    );
}

#[deterministic_id_test(matsim_rust)]
fn unknown_person_is_reported_instead_of_panicking() {
    let directory = tempfile::tempdir().unwrap();
    let mut client = start_route_service(&directory.path().join("routing-service.address"));

    let mut request: Value =
        serde_json::from_str(&car_request(HOME_LINK, WORK_LINK, START, WORK_COORD)).unwrap();
    request["person_id"] = json!("not-a-person");
    let response = client.request(&request.to_string());
    assert!(error_of(&response).contains("not-a-person"));
    assert_eq!(category_of(&response), "missing_person");

    // The same person id, once known, is routed with the agent's own subpopulation.
    let mut known: Value =
        serde_json::from_str(&car_request(HOME_LINK, WORK_LINK, START, WORK_COORD)).unwrap();
    known["person_id"] = json!("1");
    assert_eq!(client.request(&known.to_string())["error"], Value::Null);
}

#[deterministic_id_test(matsim_rust)]
fn missing_mode_and_invalid_values_are_reported() {
    let directory = tempfile::tempdir().unwrap();
    let mut client = start_route_service(&directory.path().join("routing-service.address"));

    let mut unknown_mode: Value =
        serde_json::from_str(&car_request(HOME_LINK, WORK_LINK, START, WORK_COORD)).unwrap();
    unknown_mode["mode"] = json!("motorcycle");
    let unsupported = client.request(&unknown_mode.to_string());
    assert!(
        error_of(&unsupported).contains("motorcycle"),
        "an unsupported mode should name the mode"
    );
    assert_eq!(category_of(&unsupported), "invalid_request");

    let mut negative_time: Value =
        serde_json::from_str(&car_request(HOME_LINK, WORK_LINK, START, WORK_COORD)).unwrap();
    negative_time["departure_time_seconds"] = json!(-1.0);
    let invalid = client.request(&negative_time.to_string());
    assert!(!error_of(&invalid).is_empty());
    assert_eq!(category_of(&invalid), "invalid_request");

    let malformed = client.request("{ not json");
    assert!(!error_of(&malformed).is_empty());
    assert_eq!(category_of(&malformed), "malformed_request");
    assert_eq!(
        client.request(&car_request(HOME_LINK, WORK_LINK, START, WORK_COORD))["error"],
        Value::Null
    );
}

#[deterministic_id_test(matsim_rust)]
fn disconnected_link_is_a_no_path_and_the_connection_stays_open() {
    let directory = tempfile::tempdir().unwrap();
    let mut client = start_route_service(&directory.path().join("routing-service.address"));

    // The test network has a car link that no route reaches. SILO counts this as a genuinely
    // unroutable pair rather than a defect, so it must be told apart from the other failures.
    let response = client.request(&car_request(HOME_LINK, "island", START, (30500.0, 30000.0)));
    assert!(!error_of(&response).is_empty());
    assert_eq!(category_of(&response), "no_path");
    assert_eq!(response["outcome"], "no_path");

    assert_eq!(
        client.request(&car_request(HOME_LINK, WORK_LINK, START, WORK_COORD))["error"],
        Value::Null
    );
}

#[deterministic_id_test(matsim_rust)]
fn two_partition_controller_preserves_population_events_and_routing_results() {
    let directory = tempfile::tempdir().unwrap();
    let source_network =
        std::path::Path::new("./tests/resources/equil/equil-network-with-island.xml");
    let mut candidate_network = Network::from_file_as_is(source_network);
    let owners = candidate_network
        .nodes()
        .into_iter()
        .map(|node| (node.id.clone(), u32::from(node.coord.x > 0.0)))
        .collect::<IntMap<Id<Node>, u32>>();
    candidate_network
        .apply_partition_ownership(&owners, 2)
        .unwrap();
    let candidate_network_path = directory.path().join("candidate-network.xml");
    candidate_network.to_file(&candidate_network_path);

    let baseline_output = directory.path().join("baseline");
    let candidate_output = directory.path().join("candidate");
    let diagnostics_path = directory.path().join("candidate-diagnostics.csv");
    let (mut baseline, baseline_plans) = start_route_service_with_network(
        &directory.path().join("baseline.address"),
        source_network,
        1,
        &baseline_output,
        None,
    );
    let (mut candidate, candidate_plans) = start_route_service_with_network(
        &directory.path().join("candidate.address"),
        &candidate_network_path,
        2,
        &candidate_output,
        Some(&diagnostics_path),
    );

    assert_eq!(baseline_plans, candidate_plans);
    compare_event_folder(
        baseline_output.join("ITERS/it.0/events"),
        candidate_output.join("ITERS/it.0/events"),
    )
    .unwrap();
    let route = car_request(HOME_LINK, WORK_LINK, START, WORK_COORD);
    assert_eq!(baseline.request(&route), candidate.request(&route));
    let same_link = car_request(HOME_LINK, HOME_LINK, START, START);
    assert_eq!(baseline.request(&same_link), candidate.request(&same_link));
    let malformed = candidate.request("{ not json");
    assert_eq!(category_of(&malformed), "malformed_request");
    assert_eq!(candidate.request(&route)["error"], Value::Null);

    let diagnostics = std::fs::read_to_string(diagnostics_path).unwrap();
    assert!(diagnostics.contains("worker,time_window_start_seconds,work_nanos,wait_nanos"));
    assert!(diagnostics.contains("0,0,"));
    let controller_metrics = std::fs::read_to_string(
        directory
            .path()
            .join("candidate-diagnostics.controller.json"),
    )
    .unwrap();
    assert!(controller_metrics.contains("simulation_wall_seconds"));
    assert!(controller_metrics.contains("\"mobsim\""));
}

#[deterministic_id_test(matsim_rust)]
fn two_partition_controller_preserves_supported_pt_routing_results() {
    let directory = tempfile::tempdir().unwrap();
    let config_path =
        Path::new("./tests/resources/pt_reference/routing_combined_features/config.yml");
    let source_network = Path::new("./assets/pt_tutorial/multimodalnetwork.xml");
    let population_path =
        Path::new("./tests/resources/pt_reference/routing_combined_features/population.xml");
    let mut candidate_network = Network::from_file_as_is(source_network);
    let owners = candidate_network
        .nodes()
        .into_iter()
        .map(|node| (node.id.clone(), u32::from(node.coord.x > 2500.0)))
        .collect::<IntMap<Id<Node>, u32>>();
    candidate_network
        .apply_partition_ownership(&owners, 2)
        .unwrap();
    let candidate_network_path = directory.path().join("pt-candidate-network.xml");
    candidate_network.to_file(&candidate_network_path);
    let mut baseline = start_pt_route_service(
        &directory.path().join("pt-baseline.address"),
        config_path,
        population_path,
        source_network,
        1,
        &directory.path().join("pt-baseline"),
    );
    let mut candidate = start_pt_route_service(
        &directory.path().join("pt-candidate.address"),
        config_path,
        population_path,
        &candidate_network_path,
        2,
        &directory.path().join("pt-candidate"),
    );

    let mut person_results = Vec::new();
    for person_id in ["person-1", "freight-hauler"] {
        let mut request: Value =
            serde_json::from_str(&car_request("11", "33", (1050.0, 1050.0), (3950.0, 1050.0)))
                .unwrap();
        request["mode"] = json!("pt");
        request["person_id"] = json!(person_id);
        request["departure_time_seconds"] = json!(8.0 * 3600.0);
        let baseline_result = baseline.request(&request.to_string());
        let candidate_result = candidate.request(&request.to_string());
        assert_eq!(baseline_result, candidate_result);
        assert_eq!(baseline_result["error"], Value::Null, "{baseline_result}");
        assert!(
            baseline_result["outcome"] == "pt" || baseline_result["outcome"] == "walk",
            "{baseline_result}"
        );
        assert!(baseline_result["travel_time_seconds"].as_f64().unwrap() > 0.0);
        person_results.push(baseline_result);
    }
    assert_ne!(person_results[0], person_results[1]);
}
