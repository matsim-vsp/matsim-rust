use macros::deterministic_id_test;
use matsim_rust::external_services::routing::{
    InternalRoutingRequest, InternalRoutingRequestPayload, InternalRoutingResponse,
};
use matsim_rust::external_services::{
    AdapterHandle, AdapterHandleBuilder, AsyncExecutor, ExternalServiceType, RequestAdapter,
    RequestAdapterFactory,
};
use matsim_rust::simulation::config::{CommandLineArgs, Config, StrategySettingBuilder};
use matsim_rust::simulation::controller::controller::ControllerBuilder;
use matsim_rust::simulation::controller::{ExternalServices, RequestSender};
use matsim_rust::simulation::events::utils::compare_event_folder;
use matsim_rust::simulation::id::{Id, store_to_file};
use matsim_rust::simulation::population::agent_source::PreplanningHorizonAgentSource;
use matsim_rust::simulation::replanning::DefaultStrategy;
use matsim_rust::simulation::replanning::selectors::DefaultSelector;
use matsim_rust::simulation::scenario::network::Network;
use matsim_rust::simulation::scenario::population::{
    InternalPlanElement, PREPLANNING_HORIZON, Population,
};
use matsim_rust::simulation::scenario::vehicles::Garage;
use matsim_rust::simulation::scenario::{Coordinate, Scenario};
use matsim_rust::simulation::time::SimTime;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};
use tracing::info;

// in the adaptive mod we are still using the binpb files
fn create_resources<F>(out_dir: &Path, pop_adaption: F)
where
    F: Fn(&mut Population),
{
    let input_dir = PathBuf::from("./assets/equil/");
    let net = Network::from_file_as_is(&input_dir.join("equil-network.xml"));
    let mut garage = Garage::from_file(&input_dir.join("equil-vehicles.xml"));
    let mut pop = Population::from_file(&input_dir.join("equil-1-plan.xml"), &mut garage);

    pop_adaption(&mut pop);

    store_to_file(&out_dir.join("ids.binpb"));
    net.to_file(&out_dir.join("equil-network.binpb"));
    pop.to_file(&out_dir.join("equil-1-plan.binpb"));
    garage.to_file(&out_dir.join("equil-vehicles.binpb"));
}

#[deterministic_id_test(matsim_rust)]
fn equil_single_part_matches_expected_events() {
    let config_args = CommandLineArgs::new_with_path("./tests/resources/equil/equil-config-1.yml");
    let config = Config::from_args(config_args);
    let output_dir = config.output().output_dir.clone();

    let scenario = Scenario::load(config);
    let controller = ControllerBuilder::default_with_scenario(scenario)
        .build()
        .unwrap();
    controller.run();
    compare_event_folder(
        "./tests/resources/equil/expected_events",
        output_dir.join("events"),
    )
    .unwrap();
}

#[deterministic_id_test(matsim_rust)]
fn equil_single_part_with_10_ticks_per_second_matches_expected_events() {
    let config_args = CommandLineArgs::new_with_path("./tests/resources/equil/equil-config-1.yml");
    let mut config = Config::from_args(config_args);
    config.qsim_mut().ticks_per_second = 10;
    config.output_mut().output_dir =
        PathBuf::from("./test_output/simulation/equil_single_part_10_ticks_per_second");
    let output_dir = config.output().output_dir.clone();

    let scenario = Scenario::load(config);
    let controller = ControllerBuilder::default_with_scenario(scenario)
        .build()
        .unwrap();
    controller.run();
    compare_event_folder(
        "./tests/resources/equil/expected_events_10_ticks",
        output_dir.join("events"),
    )
    .unwrap();
}

#[deterministic_id_test(matsim_rust)]
fn equil_two_parts_matches_expected_events() {
    let config_args = CommandLineArgs::new_with_path("./tests/resources/equil/equil-config-2.yml");
    let config = Config::from_args(config_args);
    let output_dir = config.output().output_dir.clone();

    let scenario = Scenario::load(config);
    let controller = ControllerBuilder::default_with_scenario(scenario)
        .build()
        .unwrap();
    controller.run();
    compare_event_folder(
        "./tests/resources/equil/expected_events",
        output_dir.join("events"),
    )
    .unwrap();
}

#[deterministic_id_test(matsim_rust)]
fn equil_full_population_single_part_runs() {
    let mut config = Config::from_args(CommandLineArgs::new_with_path(
        "./tests/resources/equil/equil-config-1.yml",
    ));
    config.population_mut().path = Some("./assets/equil/equil-plans.xml.gz".into());
    config.output_mut().output_dir =
        "./test_output/simulation/equil_full_population_single_part".into();

    config.controller_mut().last_iteration = 10;

    info!("Scoring");
    info!("{:?}", config.replanning().strategy_settings);

    config.replanning_mut().strategy_settings.clear();
    let select = StrategySettingBuilder::default()
        .name(DefaultSelector::SelectExpBeta.as_str().to_string())
        .weight(0.8)
        .subpopulation("person".to_string())
        .build()
        .unwrap();
    let reroute = StrategySettingBuilder::default()
        .name(DefaultStrategy::ReRoute.as_str().to_string())
        .weight(0.2)
        .subpopulation("person".to_string())
        .build()
        .unwrap();
    config.replanning_mut().strategy_settings.push(select);
    config.replanning_mut().strategy_settings.push(reroute);

    let scenario = Scenario::load(config);
    let controller = ControllerBuilder::default_with_scenario(scenario)
        .build()
        .unwrap();
    controller.run();
}

#[deterministic_id_test(matsim_rust)]
fn equil_full_population_two_parts_runs() {
    execute_equil_with_population(
        "./tests/resources/equil/equil-config-2.yml",
        "./assets/equil/equil-plans.xml.gz",
        "./test_output/simulation/equil_full_population_two_parts",
    );
}

#[deterministic_id_test(matsim_rust)]
#[should_panic]
fn equil_adaptive_planning_without_external_service_panics() {
    let test_dir = PathBuf::from("./test_output/simulation/equil_single_part_adaptive/");
    let config_path = "./tests/resources/equil/equil-config-1-adaptive.yml".to_string();
    let expected_events = "./tests/resources/equil/expected_events";

    // panics because no external service is provided
    execute_adaptive(
        test_dir,
        Config::from_args(CommandLineArgs::new_with_path(config_path)),
        expected_events,
        ExternalServices::default(),
        vec![],
        Arc::new(Barrier::new(1)),
    );
}

#[deterministic_id_test(matsim_rust)]
fn equil_adaptive_planning_single_part_matches_expected_events() {
    let test_dir = PathBuf::from("./test_output/simulation/equil_single_part_adaptive/");
    let config_path = "./tests/resources/equil/equil-config-1-adaptive.yml".to_string();
    let expected_events = "./tests/resources/equil/expected_events";

    let mock_routing_adapter = MockRoutingAdapterFactory::default();

    let config = Config::from_args(CommandLineArgs::new_with_path(config_path));

    let parts = config.partitioning().num_parts + 1;
    let barrier = Arc::new(Barrier::new(parts as usize));
    let executor = AsyncExecutor::from_config(&config, barrier.clone());

    let (handle, send, shutdown) = executor.spawn_thread("routing_adapter", mock_routing_adapter);

    let mut map: HashMap<ExternalServiceType, RequestSender> = HashMap::new();
    map.insert(
        ExternalServiceType::Routing("car".to_string()),
        Arc::new(send).into(),
    );

    execute_adaptive(
        test_dir,
        config,
        expected_events,
        map.into(),
        vec![
            AdapterHandleBuilder::default()
                .handle(handle)
                .shutdown_sender(shutdown)
                .build()
                .unwrap(),
        ],
        barrier,
    );
}

#[deterministic_id_test(matsim_rust)]
fn equil_adaptive_planning_two_parts_matches_expected_events() {
    let test_dir = PathBuf::from("./test_output/simulation/equil_with_channels-adaptive/");
    let config_path = "./tests/resources/equil/equil-config-2-adaptive.yml".to_string();
    let expected_events = "./tests/resources/equil/expected_events";

    let mock_routing_adapter = MockRoutingAdapterFactory::default();

    let config = Config::from_args(CommandLineArgs::new_with_path(config_path));

    let barrier = Arc::new(Barrier::new((config.partitioning().num_parts + 1) as usize));
    let executor = AsyncExecutor::from_config(&config, barrier.clone());

    let (handle, send, shutdown) = executor.spawn_thread("routing_adapter", mock_routing_adapter);

    let mut map: HashMap<ExternalServiceType, RequestSender> = HashMap::new();
    map.insert(
        ExternalServiceType::Routing("car".to_string()),
        Arc::new(send).into(),
    );

    execute_adaptive(
        test_dir,
        config,
        expected_events,
        map.into(),
        vec![
            AdapterHandleBuilder::default()
                .handle(handle)
                .shutdown_sender(shutdown)
                .build()
                .unwrap(),
        ],
        barrier,
    );
}

#[derive(Default)]
struct MockRoutingAdapterFactory {}

impl RequestAdapterFactory<InternalRoutingRequest> for MockRoutingAdapterFactory {
    async fn build(self) -> impl RequestAdapter<InternalRoutingRequest> {
        MockRoutingAdapter::default()
    }
}

#[derive(Default)]
struct MockRoutingAdapter {
    requests: Vec<InternalRoutingRequestPayload>,
}

impl RequestAdapter<InternalRoutingRequest> for MockRoutingAdapter {
    fn on_request(&mut self, req: InternalRoutingRequest) {
        self.requests.push(req.payload);
        req.response_tx
            .send(InternalRoutingResponse::default())
            .unwrap();
    }

    fn on_shutdown(&mut self) {
        assert_eq!(self.requests.len(), 1);
        assert!(
            self.requests[0].equals_ignoring_uuid(&InternalRoutingRequestPayload {
                person_id: "1".to_string(),
                from_link: "1".to_string(),
                from: Coordinate::new_2d(-25000., 0.),
                to_link: "20".to_string(),
                to: Coordinate::new_2d(3456., 4242.),
                mode: "car".to_string(),
                departure_time: SimTime::from_secs(21600),
                now: SimTime::from_secs(21000),
                uuid: Default::default(),
            })
        );
    }
}

fn execute_equil_with_population(
    config_path: impl Into<String>,
    population_path: impl Into<PathBuf>,
    output_dir: impl Into<PathBuf>,
) {
    let mut config = Config::from_args(CommandLineArgs::new_with_path(config_path.into()));
    config.population_mut().path = Some(population_path.into());
    config.output_mut().output_dir = output_dir.into();

    let scenario = Scenario::load(config);
    let controller = ControllerBuilder::default_with_scenario(scenario)
        .build()
        .unwrap();
    controller.run();
}

fn execute_adaptive(
    test_dir: PathBuf,
    config: Config,
    expected_events: &str,
    map: ExternalServices,
    adapter_handles: Vec<AdapterHandle>,
    global_barrier: Arc<Barrier>,
) {
    let f = |pop: &mut Population| {
        let agent = pop.persons.get_mut(&Id::create("1")).unwrap();
        let plan = agent.selected_plan_mut();
        match plan.elements.get_mut(0).unwrap() {
            InternalPlanElement::Activity(a) => {
                // add a preplanning horizon attribute to
                a.attributes.insert(PREPLANNING_HORIZON, 10 * 60);
            }
            InternalPlanElement::Leg(_) => {
                panic!()
            }
        }
    };

    create_resources(&test_dir, f);

    let output_dir = config.output().output_dir.clone();
    let scenario = Scenario::load(config);
    let controller = ControllerBuilder::default_with_scenario(scenario)
        .agent_source(PreplanningHorizonAgentSource)
        .external_services(map)
        .adapter_handles(adapter_handles)
        .global_barrier(global_barrier)
        .build()
        .unwrap();
    controller.run();
    compare_event_folder(expected_events, output_dir.join("events")).unwrap();
}
