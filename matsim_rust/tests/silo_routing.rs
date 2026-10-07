//! The route service that SILO queries after QSim has finished.
//!
//! SILO resolves origin and destination to network links itself, so a request may name
//! links or persons this population never loaded. Those are ordinary bad requests and
//! must not take the service down with them.

use macros::deterministic_id_test;
use matsim_rust::external_services::silo_routing;
use matsim_rust::simulation::config::{CommandLineArgs, Config};
use matsim_rust::simulation::controller::controller::ControllerBuilder;
use matsim_rust::simulation::scenario::Scenario;
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
    assert!(error_of(&client.request(&request.to_string())).contains("pt"));
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

    assert_eq!(
        client.request(&car_request(HOME_LINK, WORK_LINK, START, WORK_COORD))["error"],
        Value::Null
    );
}
