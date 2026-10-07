use crate::simulation::id::Id;
use crate::simulation::replanning::routing::Facility;
use crate::simulation::replanning::routing::{RoutingError, RoutingRequestBuilder, TripRouter};
use crate::simulation::scenario::Coordinate;
use crate::simulation::scenario::network::Link;
use crate::simulation::scenario::population::{InternalPerson, InternalPlanElement, Population};
use crate::simulation::time::SimTime;
use crate::simulation::time::time_interpretation::TimeInterpretation;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

#[derive(Debug, Deserialize)]
struct RouteRequest {
    mode: String,
    from_x: f64,
    from_y: f64,
    from_link_id: String,
    to_x: f64,
    to_y: f64,
    to_link_id: String,
    departure_time_seconds: f64,
    person_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct RouteResponse {
    travel_time_seconds: Option<f64>,
    distance_meters: Option<f64>,
    error: Option<String>,
    failure_category: Option<&'static str>,
}

impl RouteResponse {
    fn error(category: &'static str, message: impl Into<String>) -> Self {
        Self {
            travel_time_seconds: None,
            distance_meters: None,
            error: Some(message.into()),
            failure_category: Some(category),
        }
    }
}

/// Serves SILO's repeated route queries after QSim, keeping the loaded Rust router and its
/// simulated travel-time snapshot alive for the rest of the SILO year.
pub fn serve(
    router: TripRouter,
    population: Population,
    bind: &str,
    ready_file: &Path,
) -> Result<(), String> {
    let listener =
        TcpListener::bind(bind).map_err(|e| format!("Could not bind route service: {e}"))?;
    let address = listener
        .local_addr()
        .map_err(|e| format!("Could not read route service address: {e}"))?;
    if let Some(parent) = ready_file.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Could not create route service marker directory: {e}"))?;
    }
    fs::write(ready_file, address.to_string())
        .map_err(|e| format!("Could not write route service marker: {e}"))?;

    let router = Arc::new(router);
    let population = Arc::new(population);
    for connection in listener.incoming() {
        match connection {
            Ok(stream) => {
                let router = router.clone();
                let population = population.clone();
                thread::spawn(move || handle_connection(stream, router, population));
            }
            Err(error) => return Err(format!("Route service accept failed: {error}")),
        }
    }
    Ok(())
}

fn handle_connection(stream: TcpStream, router: Arc<TripRouter>, population: Arc<Population>) {
    let Ok(reader_stream) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(reader_stream);
    let mut writer = BufWriter::new(stream);
    let mut line = String::new();

    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }

        let response = match serde_json::from_str::<RouteRequest>(&line) {
            Ok(request) => route(&router, &population, request),
            Err(error) => RouteResponse::error(
                "malformed_request",
                format!("Invalid route request: {error}"),
            ),
        };
        if serde_json::to_writer(&mut writer, &response).is_err()
            || writer.write_all(b"\n").is_err()
            || writer.flush().is_err()
        {
            return;
        }
    }
}

fn route(router: &TripRouter, population: &Population, request: RouteRequest) -> RouteResponse {
    if !request.departure_time_seconds.is_finite()
        || request.departure_time_seconds < 0.0
        || !request.from_x.is_finite()
        || !request.from_y.is_finite()
        || !request.to_x.is_finite()
        || !request.to_y.is_finite()
    {
        return RouteResponse::error(
            "invalid_request",
            "Route request contains an invalid coordinate or time",
        );
    }

    let departure = Duration::from_secs_f64(request.departure_time_seconds);
    let departure_time = SimTime::from_duration(departure);
    // SILO resolves origin and destination to network links itself, so an id that this
    // population never loaded is an ordinary bad request. Resolving it with
    // `Id::get_from_ext` would panic and drop the connection instead of answering.
    let from_link = match Id::<Link>::try_get_from_ext(&request.from_link_id) {
        Some(link) => link,
        None => {
            return RouteResponse::error(
                "invalid_link",
                format!(
                    "Link `{}` is not in the routed network",
                    request.from_link_id
                ),
            );
        }
    };
    let to_link = match Id::<Link>::try_get_from_ext(&request.to_link_id) {
        Some(link) => link,
        None => {
            return RouteResponse::error(
                "invalid_link",
                format!("Link `{}` is not in the routed network", request.to_link_id),
            );
        }
    };
    let from = Facility::new_link_wrapper(
        Coordinate::new_2d(request.from_x, request.from_y),
        from_link.clone(),
    );
    let to = Facility::new_link_wrapper(
        Coordinate::new_2d(request.to_x, request.to_y),
        to_link.clone(),
    );
    let mode = Id::<String>::create(&request.mode);
    let person = match request.person_id.as_deref() {
        Some(person_id) => match Id::<InternalPerson>::try_get_from_ext(person_id) {
            Some(person_id) => match population.persons.get(&person_id) {
                Some(person) => Some(person),
                None => {
                    return RouteResponse::error(
                        "missing_person",
                        format!("Person `{person_id}` is not in the routed population"),
                    );
                }
            },
            None => {
                return RouteResponse::error(
                    "missing_person",
                    format!("Person `{person_id}` is not in the routed population"),
                );
            }
        },
        None => None,
    };
    let routing_request = match RoutingRequestBuilder::default()
        .from(&from)
        .to(&to)
        .departure_time(departure_time)
        .person(person)
        .build()
    {
        Ok(request) => request,
        Err(error) => {
            return RouteResponse::error(
                "invalid_request",
                format!("Invalid route request: {error}"),
            );
        }
    };
    let elements = match router.calc_route(&mode, routing_request) {
        Ok(elements) => elements,
        Err(error) => {
            let category = match error {
                RoutingError::NoPath { .. } => "no_path",
                RoutingError::MissingModule { .. } | RoutingError::Unsupported { .. } => {
                    "invalid_request"
                }
                RoutingError::MissingEndTime { .. } => "service_error",
            };
            return RouteResponse::error(category, error.to_string());
        }
    };
    let Some(arrival) = TimeInterpretation::decide_on_elements_end_time(&elements, &departure_time)
    else {
        return RouteResponse::error(
            "service_error",
            "Rust router returned route elements without an arrival time",
        );
    };
    let distance = elements
        .iter()
        .filter_map(|element| match element {
            InternalPlanElement::Leg(leg) => leg
                .route
                .as_ref()
                .and_then(|route| route.as_generic().distance()),
            InternalPlanElement::Activity(_) => None,
        })
        .sum();

    RouteResponse {
        travel_time_seconds: Some(arrival.duration_since(departure_time).as_secs_f64()),
        distance_meters: Some(distance),
        error: None,
        failure_category: None,
    }
}

pub fn parse_address(value: &str) -> Result<SocketAddr, String> {
    value
        .parse()
        .map_err(|e| format!("Invalid route service address `{value}`: {e}"))
}
