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
    outcome: Option<&'static str>,
}

impl RouteResponse {
    fn error(category: &'static str, message: impl Into<String>) -> Self {
        Self {
            travel_time_seconds: None,
            distance_meters: None,
            error: Some(message.into()),
            failure_category: Some(category),
            outcome: (category == "no_path").then_some("no_path"),
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
                // The person exists but its loaded attributes cannot be read, which is this
                // service's own input data and not something SILO sent.
                RoutingError::MissingEndTime { .. } | RoutingError::MalformedAttribute { .. } => {
                    "service_error"
                }
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
    let outcome = if elements.iter().any(|element| {
        element
            .as_leg()
            .is_some_and(|leg| leg.mode.external() == "pt")
    }) {
        "pt"
    } else if elements.iter().any(|element| {
        element
            .as_leg()
            .is_some_and(|leg| leg.mode.external() == "walk")
    }) {
        "walk"
    } else {
        "other"
    };

    RouteResponse {
        travel_time_seconds: Some(arrival.duration_since(departure_time).as_secs_f64()),
        distance_meters: Some(distance),
        error: None,
        failure_category: None,
        outcome: Some(outcome),
    }
}

pub fn parse_address(value: &str) -> Result<SocketAddr, String> {
    value
        .parse()
        .map_err(|e| format!("Invalid route service address `{value}`: {e}"))
}

#[cfg(test)]
mod tests {
    use super::{RouteRequest, route};
    use crate::simulation::InternalAttributes;
    use crate::simulation::id::Id;
    use crate::simulation::replanning::routing::{RoutingModule, TransitRoutingModule, TripRouter};
    use crate::simulation::scenario::population::{InternalPerson, InternalPlan, Population};
    use crate::simulation::scenario::transit::TransitSchedule;
    use crate::simulation::scenario::vehicles::Garage;
    use macros::deterministic_id_test;
    use nohash_hasher::IntMap;
    use serde_json::json;
    use std::sync::Arc;

    /// A car router that cannot answer, so reaching it is observable as the service error its
    /// [`super::RoutingError::Unsupported`] maps to. A car trip is therefore
    /// distinguishable from a no-path answer only by which one happened.
    struct CarStub;

    impl RoutingModule for CarStub {
        fn calc_route(
            &self,
            _request: crate::simulation::replanning::routing::RoutingRequest,
        ) -> Result<
            Vec<crate::simulation::scenario::population::InternalPlanElement>,
            crate::simulation::replanning::routing::RoutingError,
        > {
            Err(
                crate::simulation::replanning::routing::RoutingError::Unsupported {
                    mode: "car".to_string(),
                },
            )
        }

        fn mode(&self) -> &Id<String> {
            static MODE: std::sync::OnceLock<Id<String>> = std::sync::OnceLock::new();
            MODE.get_or_init(|| Id::create("car"))
        }
    }

    /// The pt router as the controller wires it: a transit module whose fallback is the car
    /// router. The empty schedule connects nothing, so every `pt` query reaches the gate.
    fn pt_router_with_car_fallback(personless_fallback: bool) -> TripRouter {
        // `route` resolves the request's links through the global ID store and answers
        // `invalid_link` for one it never loaded, so the two links the requests below name have
        // to exist. `#[deterministic_id_test]` resets that store before each test.
        Id::<crate::simulation::scenario::network::Link>::create("1");
        Id::<crate::simulation::scenario::network::Link>::create("5");

        let pt = TransitRoutingModule::new(
            Arc::new(TransitSchedule::default()),
            1.0,
            1.0,
            Arc::new(Garage::default()),
            Some(Arc::new(CarStub)),
        )
        .with_personless_fallback(personless_fallback);
        let mut modules: IntMap<Id<String>, Arc<dyn RoutingModule>> = IntMap::default();
        modules.insert(Id::create("pt"), Arc::new(pt));
        TripRouter::new(modules)
    }

    fn pt_request(person_id: Option<&str>) -> RouteRequest {
        RouteRequest {
            mode: "pt".to_string(),
            from_x: 0.0,
            from_y: 0.0,
            from_link_id: "1".to_string(),
            to_x: 10.0,
            to_y: 10.0,
            to_link_id: "5".to_string(),
            departure_time_seconds: 0.0,
            person_id: person_id.map(str::to_string),
        }
    }

    fn population_with(owns_car: Option<serde_json::Value>) -> Population {
        let mut person = InternalPerson::new(
            Id::create("owner"),
            InternalPlan {
                score: None,
                selected: true,
                elements: Vec::new(),
                attributes: InternalAttributes::default(),
            },
        );
        if let Some(owns_car) = owns_car {
            person.attributes_mut().insert("ownsCar", owns_car);
        }
        let mut population = Population::new();
        population.persons.insert(person.id().clone(), person);
        population
    }

    /// SILO resolves a zone pair to links itself and sends no person with it. That query names
    /// no agent, so nothing can have declared car ownership, and the service must not invent a
    /// car trip: it reports the ordinary no-path answer. The `CarStub` fallback would have
    /// answered `service_error`, so this also proves the car router was not consulted.
    #[deterministic_id_test]
    fn personless_pt_query_reports_no_path_instead_of_a_car_trip() {
        let response = route(
            &pt_router_with_car_fallback(false),
            &Population::new(),
            pt_request(None),
        );

        assert_eq!(response.failure_category, Some("no_path"));
        assert_eq!(response.outcome, Some("no_path"));
        assert_eq!(
            serde_json::to_value(&response).unwrap()["outcome"],
            "no_path"
        );
        assert!(
            !response
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("service_error"),
            "the car fallback was consulted: {:?}",
            response.error
        );
    }

    #[deterministic_id_test]
    fn personless_pt_query_exposes_transit_as_the_success_outcome() {
        Id::<crate::simulation::scenario::network::Link>::create("11");
        Id::<crate::simulation::scenario::network::Link>::create("33");
        let schedule = TransitSchedule::from_file(
            "./tests/resources/pt_reference/routing_direct_vs_transfer/transit_schedule.xml"
                .as_ref(),
        );
        let pt = TransitRoutingModule::new(
            Arc::new(schedule),
            0.8333333333333334,
            1.0,
            Arc::new(Garage::default()),
            None,
        );
        let mut modules: IntMap<Id<String>, Arc<dyn RoutingModule>> = IntMap::default();
        modules.insert(Id::create("pt"), Arc::new(pt));
        let mut request = pt_request(None);
        request.from_link_id = "11".to_string();
        request.from_x = 1050.0;
        request.from_y = 1050.0;
        request.to_link_id = "33".to_string();
        request.to_x = 3950.0;
        request.to_y = 1050.0;
        request.departure_time_seconds = 8.0 * 3600.0;

        let response = route(&TripRouter::new(modules), &Population::new(), request);

        assert_eq!(response.outcome, Some("pt"), "{response:?}");
        assert_eq!(response.failure_category, None);
        assert!(response.travel_time_seconds.is_some());
        assert_eq!(serde_json::to_value(&response).unwrap()["outcome"], "pt");
    }

    #[deterministic_id_test]
    fn personless_pt_query_exposes_direct_walking_as_the_success_outcome() {
        Id::<crate::simulation::scenario::network::Link>::create("11");
        Id::<crate::simulation::scenario::network::Link>::create("12");
        let mut schedule = TransitSchedule::from_file(
            "./tests/resources/pt_reference/routing_direct_vs_transfer/transit_schedule.xml"
                .as_ref(),
        );
        for route in schedule
            .lines_mut()
            .values_mut()
            .flat_map(|line| line.routes.values_mut())
        {
            route.departures.clear();
        }
        let pt = TransitRoutingModule::new(
            Arc::new(schedule),
            0.8333333333333334,
            1.0,
            Arc::new(Garage::default()),
            None,
        );
        let mut modules: IntMap<Id<String>, Arc<dyn RoutingModule>> = IntMap::default();
        modules.insert(Id::create("pt"), Arc::new(pt));
        let mut request = pt_request(None);
        request.from_link_id = "11".to_string();
        request.from_x = 1050.0;
        request.from_y = 2940.0;
        request.to_link_id = "12".to_string();
        request.to_x = 2050.0;
        request.to_y = 2940.0;
        request.departure_time_seconds = 8.0 * 3600.0;

        let response = route(&TripRouter::new(modules), &Population::new(), request);

        assert_eq!(response.outcome, Some("walk"));
        assert_eq!(response.failure_category, None);
        assert_eq!(response.travel_time_seconds, Some(1200.0));
        assert_eq!(serde_json::to_value(&response).unwrap()["outcome"], "walk");
    }

    /// A personless query may still be answered with a car trip, but only when the integrator
    /// opted in. This is the legacy SILO behaviour, kept separate from passenger semantics.
    ///
    /// The stub reports `Unsupported`, which the service maps to `invalid_request`. That is the
    /// point of the assertion: it differs from the `no_path` the default produces, so it shows
    /// the gate admitted the query to the car router rather than denying it.
    #[deterministic_id_test]
    fn personless_pt_query_reaches_the_car_fallback_when_configured() {
        let response = route(
            &pt_router_with_car_fallback(true),
            &Population::new(),
            pt_request(None),
        );

        assert_eq!(response.failure_category, Some("invalid_request"));
        assert!(
            response
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("car"),
            "the failure should name the car router the stub reported: {:?}",
            response.error
        );
    }

    /// A passenger whose ownership attribute cannot be read is a bad input on this service's
    /// own side, not something SILO sent. It is reported as a service error rather than
    /// silently read as "does not own a car", which would hand the agent no trip and hide the
    /// broken attribute behind an ordinary no-path answer.
    #[deterministic_id_test]
    fn malformed_ownership_is_a_service_error_not_a_no_path() {
        let population = population_with(Some(json!("yes")));
        let response = route(
            &pt_router_with_car_fallback(false),
            &population,
            pt_request(Some("owner")),
        );

        assert_eq!(response.failure_category, Some("service_error"));
        let message = response.error.as_deref().unwrap_or_default();
        assert!(message.contains("ownsCar"), "{message}");
        assert!(message.contains("owner"), "{message}");
    }

    /// Declared ownership without a car to drive is the walking/no-path outcome, and it is
    /// distinct from the malformed case above.
    #[deterministic_id_test]
    fn declared_ownership_without_a_car_reports_no_path() {
        let population = population_with(Some(json!(true)));
        let response = route(
            &pt_router_with_car_fallback(false),
            &population,
            pt_request(Some("owner")),
        );

        assert_eq!(response.failure_category, Some("no_path"));
    }

    /// Missing ownership denies the fallback just like an explicit false: a generated
    /// `{person}_car` vehicle is not a declaration.
    #[deterministic_id_test]
    fn missing_ownership_reports_no_path() {
        let population = population_with(None);
        let response = route(
            &pt_router_with_car_fallback(false),
            &population,
            pt_request(Some("owner")),
        );

        assert_eq!(response.failure_category, Some("no_path"));
    }
}
