use crate::external_services::AdapterHandle;
use crate::simulation::config::{Config, Logging, OverwriteFiles, WriteEvents, write_config};
use crate::simulation::controller::{
    ExternalServices, MobsimWorkerPool, MobsimWorkerPoolArgumentsBuilder, ReplanningPool,
    ScoringPool, create_output_filename,
};
use crate::simulation::framework_events::{
    ControllerEvent, ControllerEventsManager, ControllerListenerRegisterFn,
    WorkerListenerRegisterFunction,
};
use crate::simulation::id::Id;
use crate::simulation::logging::init_controller_logging;
use crate::simulation::network::LinkStorageCapacities;
use crate::simulation::population::agent_source::{
    DynAgentSource, IntoDynAgentSource, PopulationAgentSource,
};
use crate::simulation::replanning::ReplanningStrategy;
use crate::simulation::replanning::routing::a_star::{AStar, AltHeuristic};
use crate::simulation::replanning::routing::cost::ScoringBasedTravelTimeAndDisutility;
use crate::simulation::replanning::routing::network_routing::NetworkRoutingModule;
use crate::simulation::replanning::routing::teleportation::TeleportationRoutingModule;
use crate::simulation::replanning::routing::travel_time_calculator::{
    GlobalTravelTimeCalculator, PartitionTravelTimeCollector,
};
use crate::simulation::replanning::routing::{RoutingModule, TransitRoutingModule, TripRouter};
use crate::simulation::scenario::population::Population;
use crate::simulation::scenario::prepare::prepare_for_mobsim::prepare_for_mobsim;
use crate::simulation::scenario::prepare::prepare_for_sim::prepare_for_sim;
use crate::simulation::scenario::{ControllerScenario, Scenario};
use crate::simulation::scoring;
use crate::simulation::scoring::{PersonExperiences, PlanScorer};
use crate::simulation::{id, io};
use derive_more::Debug;
use fs_extra::dir::CopyOptions;
use itertools::Itertools;
use nohash_hasher::IntMap;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};
use std::{fs, mem};
use tracing::info;

#[derive(Debug)]
pub struct Controller {
    scenario: ControllerScenario,
    link_storage_capacities: LinkStorageCapacities,
    config: Arc<Config>,
    #[debug(skip)]
    agent_source: DynAgentSource,
    controller_events_manager: ControllerEventsManager,
    #[debug(skip)]
    worker_listener: HashMap<u32, Vec<Box<WorkerListenerRegisterFunction>>>,
    external_services: ExternalServices,
    global_barrier: Arc<Barrier>,
    adapter_handles: Vec<AdapterHandle>,
    trip_router: TripRouter,
    expected_travel: Vec<crate::simulation::analysis::PersonExpectedTravel>,
    #[debug(skip)]
    experienced_plan_collection: scoring::ExperiencedPlansCollection,
    #[debug(skip)]
    scoring_function: Option<Box<dyn PlanScorer>>,
    #[debug(skip)]
    replanning_strategies: Vec<Box<dyn ReplanningStrategy>>,
    person_demographics: Vec<crate::simulation::analysis::PersonDemographic>,
}

pub struct ControllerBuilder {
    scenario: Scenario,
    agent_source: DynAgentSource,
    controller_event_register_fn: Vec<Box<ControllerListenerRegisterFn>>,
    worker_listener_register_fn: HashMap<u32, Vec<Box<WorkerListenerRegisterFunction>>>,
    external_services: ExternalServices,
    global_barrier: Option<Arc<Barrier>>,
    adapter_handles: Vec<AdapterHandle>,
    scoring_function: Option<Box<dyn PlanScorer>>,
    replanning_strategies: Vec<Box<dyn ReplanningStrategy>>,
}

impl ControllerBuilder {
    pub fn default_with_scenario(scenario: Scenario) -> Self {
        ControllerBuilder {
            scenario,
            agent_source: Arc::new(PopulationAgentSource),
            controller_event_register_fn: Vec::new(),
            worker_listener_register_fn: HashMap::new(),
            external_services: ExternalServices::default(),
            global_barrier: None,
            adapter_handles: Vec::new(),
            scoring_function: None,
            replanning_strategies: Vec::new(),
        }
    }

    // Implementing a custom build function in order to set the barrier if not set by the user.
    pub fn build(mut self) -> Result<Controller, String> {
        self.scenario.config.transit().validate()?;
        self.scenario.config.travel_time_calculator().validate()?;
        let transit = self.scenario.config.transit();
        if transit.use_mode_mapping_for_passengers
            || !transit.mode_mapping_for_passengers.is_empty()
        {
            if transit.use_mode_mapping_for_passengers
                && transit.mode_mapping_for_passengers.is_empty()
            {
                return Err("transit.mode_mapping_for_passengers must not be empty when passenger mode mapping is enabled".to_owned());
            }
            for (route_mode, passenger_mode) in &transit.mode_mapping_for_passengers {
                if route_mode.trim().is_empty() || passenger_mode.trim().is_empty() {
                    return Err("transit.mode_mapping_for_passengers routeMode and passengerMode must not be empty".to_owned());
                }
                if !self.scenario.transit_schedule.lines().values().any(|line| {
                    line.routes
                        .values()
                        .any(|route| route.transport_mode.external() == route_mode)
                }) {
                    return Err(format!(
                        "transit.mode_mapping_for_passengers routeMode {route_mode} is not present in the transit schedule"
                    ));
                }
                if transit.use_mode_mapping_for_passengers {
                    if !transit.transit_modes.contains(passenger_mode) {
                        return Err(format!(
                            "transit.mode_mapping_for_passengers passengerMode {passenger_mode} must be listed in transit.transit_modes"
                        ));
                    }
                    let Some(params) = self
                        .scenario
                        .config
                        .scoring()
                        .mode_params
                        .iter()
                        .find(|params| params.mode == *passenger_mode)
                    else {
                        return Err(format!(
                            "transit.mode_mapping_for_passengers passengerMode {passenger_mode} needs scoring mode parameters"
                        ));
                    };
                    if let Some((field, _)) = [
                        (
                            "marginal_utility_of_traveling",
                            params.marginal_utility_of_traveling,
                        ),
                        (
                            "marginal_utility_of_distance",
                            params.marginal_utility_of_distance,
                        ),
                        (
                            "monetary_distance_cost_rate",
                            params.monetary_distance_cost_rate,
                        ),
                        ("daily_money_constant", params.daily_money_constant),
                        ("daily_utility_constant", params.daily_utility_constant),
                        ("constant", params.constant),
                    ]
                    .into_iter()
                    .find(|(_, value)| !value.is_finite())
                    {
                        return Err(format!(
                            "scoring mode {passenger_mode} has a non-finite {field}"
                        ));
                    }
                    let performing = self
                        .scenario
                        .config
                        .scoring()
                        .agent_params
                        .iter()
                        .find(|params| params.subpopulation == "person")
                        .map_or(6.0, |params| params.performing);
                    let pt_utility = self
                        .scenario
                        .config
                        .scoring()
                        .mode_params
                        .iter()
                        .find(|params| params.mode == "pt")
                        .map_or(-6.0, |params| params.marginal_utility_of_traveling);
                    if !performing.is_finite() || !pt_utility.is_finite() {
                        return Err(
                            "scoring parameters for mapped transit routing must be finite"
                                .to_owned(),
                        );
                    }
                    if performing == pt_utility {
                        return Err(
                            "scoring parameters for pt must produce a non-zero travel-time cost"
                                .to_owned(),
                        );
                    }
                }
            }
        }
        let num_parts = self.scenario.config.partitioning().num_parts;
        let bin_size = self.scenario.config.travel_time_calculator().bin_size;
        let end_time = self.scenario.config.qsim().end_time;

        // create a barrier for the number of partitions, if not provided
        let barrier = self
            .global_barrier
            .take()
            .unwrap_or_else(|| Arc::new(Barrier::new(num_parts as usize)));

        let mut controller_event_manager = ControllerEventsManager::default();
        for register_fn in self.controller_event_register_fn {
            register_fn(&mut controller_event_manager);
        }

        // Prepare the scenario once, before it is shared, e.g. with the routing modules. The
        // scoring registrations below also need the resolved activity links.
        prepare_for_sim(&mut self.scenario)
            .unwrap_or_else(|err| panic!("{err}: {:?}", err.issues()));

        let link_storage_capacities = LinkStorageCapacities::from_network(
            &self.scenario.network,
            self.scenario.config.qsim(),
        );
        let scenario: ControllerScenario = self.scenario.into();
        let config = scenario.core.config.clone();

        let (worker_registrations, controller_registration, experienced_plans) =
            scoring::create_registrations(&scenario);
        for (rank, registrations) in worker_registrations {
            self.worker_listener_register_fn
                .entry(rank)
                .or_default()
                .extend(registrations);
        }
        controller_registration(&mut controller_event_manager);

        let global_ttc = Arc::new(GlobalTravelTimeCalculator::new(
            num_parts as usize,
            Duration::from_secs(u64::from(config.travel_time_calculator().bin_size)),
            Duration::from_secs(u64::from(config.qsim().end_time)),
        ));
        let router = Self::create_trip_router(config.as_ref(), &scenario, global_ttc.clone())?;

        for i in 0..num_parts {
            let net = scenario.core.network.clone();
            let global_ttc = global_ttc.clone();
            let travel_time_collector = move || {
                Rc::new(RefCell::new(PartitionTravelTimeCollector::new(
                    Duration::from_secs(u64::from(bin_size)),
                    Duration::from_secs(u64::from(end_time)),
                )))
            };
            self.worker_listener_register_fn
                .entry(i)
                .or_default()
                .push(Box::new(move |events, mobsim, partition, _migration| {
                    let ttc = travel_time_collector();
                    PartitionTravelTimeCollector::register_events(&ttc, events);
                    PartitionTravelTimeCollector::register_travel_time_publication(
                        &ttc, global_ttc, net, i, mobsim,
                    );
                    PartitionTravelTimeCollector::register_partition_events(&ttc, partition);
                }));
        }

        Ok(Controller {
            scenario,
            link_storage_capacities,
            config,
            agent_source: self.agent_source,
            controller_events_manager: controller_event_manager,
            worker_listener: self.worker_listener_register_fn,
            external_services: self.external_services,
            global_barrier: barrier,
            adapter_handles: self.adapter_handles,
            trip_router: router,
            expected_travel: Vec::new(),
            experienced_plan_collection: experienced_plans,
            scoring_function: self.scoring_function,
            replanning_strategies: self.replanning_strategies,
            person_demographics: Vec::new(),
        })
    }

    pub fn controller_event_register_fn(
        mut self,
        v: Vec<Box<ControllerListenerRegisterFn>>,
    ) -> Self {
        self.controller_event_register_fn = v;
        self
    }

    pub fn agent_source(mut self, source: impl IntoDynAgentSource) -> Self {
        self.agent_source = source.into_dyn_agent_source();
        self
    }

    pub fn scoring_function(mut self, scoring_function: Box<dyn PlanScorer>) -> Self {
        self.scoring_function = Some(scoring_function);
        self
    }

    pub fn external_services(mut self, e: ExternalServices) -> Self {
        self.external_services = e;
        self
    }

    pub fn global_barrier(mut self, b: Arc<Barrier>) -> Self {
        self.global_barrier = Some(b);
        self
    }

    pub fn adapter_handles(mut self, v: Vec<AdapterHandle>) -> Self {
        self.adapter_handles = v;
        self
    }

    /// Registers a named replanning strategy that can be selected from the scenario config.
    pub fn replanning_strategy(mut self, strategy: Box<dyn ReplanningStrategy>) -> Self {
        self.replanning_strategies.push(strategy);
        self
    }

    pub fn worker_listener_register_fn(
        mut self,
        worker_listener_register_fn: HashMap<u32, Vec<Box<WorkerListenerRegisterFunction>>>,
    ) -> Self {
        self.worker_listener_register_fn = worker_listener_register_fn;
        self
    }

    fn create_trip_router(
        config: &Config,
        controller_scenario: &ControllerScenario,
        global_ttc: Arc<GlobalTravelTimeCalculator>,
    ) -> Result<TripRouter, String> {
        let mut routers: IntMap<Id<String>, Arc<dyn RoutingModule>> = IntMap::default();

        // for every teleported mode, create the corresponding router.
        for t in &config.routing().teleported_mode_params {
            let id = Id::create(&t.mode);

            let module = Arc::new(TeleportationRoutingModule::new(
                id.clone(),
                t.beeline_distance_factor,
                t.teleported_mode_speed,
            ));

            routers.insert(id, module);
        }

        let access_egress_mode = Id::create(&config.routing().access_egress_mode);

        // for every configured network mode, create the corresponding router.
        for mode in &config.routing().network_modes {
            let id = Id::create(mode);
            let Some(access_egress) = routers.get(&access_egress_mode).cloned() else {
                return Err(format!(
                    "No {} access/egress router found for mode {}. Please ensure that the teleported mode params include the configured access/egress mode.",
                    access_egress_mode.external(),
                    id.external(),
                ));
            };
            let time_utility = Arc::new(ScoringBasedTravelTimeAndDisutility::new(
                config,
                id.clone(),
                global_ttc.clone(),
            ));
            let astar = AStar::<AltHeuristic>::new(
                controller_scenario.core.network.clone(),
                Some(id.clone()),
                time_utility.clone(),
                time_utility,
            )
            .map_err(|error| {
                format!(
                    "Failed to create network router for mode {}: {error}",
                    id.external()
                )
            })?;

            let module: Arc<dyn RoutingModule> = Arc::new(NetworkRoutingModule::new(
                id.clone(),
                access_egress,
                Box::new(astar),
                controller_scenario.core.clone(),
            ));

            routers.insert(id, module);
        }

        if !controller_scenario.core.transit_schedule.lines().is_empty() {
            let walk = config
                .routing()
                .teleported_mode_params
                .iter()
                .find(|params| params.mode == "walk")
                .expect("routing config always includes walk parameters");
            let mode = Id::create("pt");
            let car_fallback = routers.get(&Id::create("car")).cloned();
            let feeder_routers = routers.clone();
            let mut transit_router = TransitRoutingModule::new_with_transfer_construction(
                controller_scenario.core.transit_schedule.clone(),
                walk.teleported_mode_speed,
                walk.beeline_distance_factor,
                controller_scenario.core.garage.clone(),
                car_fallback,
                config.transit().transfer_construction,
            )
            .with_personless_fallback(config.transit().personless_car_fallback)
            .with_passenger_mode_mapping(
                config.transit().use_mode_mapping_for_passengers,
                config.transit().mode_mapping_for_passengers.clone(),
                &config.scoring().mode_params,
                &config.scoring().agent_params,
            )
            .with_range_queries(
                config.transit().range_query_settings.clone(),
                config.transit().route_selector_settings.clone(),
                config.computational_setup().random_seed,
            );
            if config.transit().use_intermodal_access_egress {
                if config.transit().intermodal_access_egress.is_empty() {
                    return Err("transit.use_intermodal_access_egress requires at least one transit.intermodal_access_egress entry".to_owned());
                }
                for setting in &config.transit().intermodal_access_egress {
                    if setting.mode.is_empty()
                        || !setting.initial_search_radius.is_finite()
                        || setting.initial_search_radius <= 0.0
                        || setting.max_radius.is_nan()
                        || setting.max_radius < setting.initial_search_radius
                        || !setting.search_extension_radius.is_finite()
                        || setting.search_extension_radius <= 0.0
                        || setting.share_trip_search_radius.is_nan()
                        || setting.share_trip_search_radius <= 0.0
                    {
                        return Err(format!(
                            "Invalid transit intermodal access/egress settings for mode '{}': search radii and share_trip_search_radius must be positive, and max_radius must be at least initial_search_radius",
                            setting.mode
                        ));
                    }
                    if !feeder_routers.contains_key(&Id::create(&setting.mode)) {
                        return Err(format!(
                            "No routing module found for configured transit feeder mode '{}'",
                            setting.mode
                        ));
                    }
                    if setting.person_filter_attribute.is_some()
                        != setting.person_filter_value.is_some()
                        || setting.stop_filter_attribute.is_some()
                            != setting.stop_filter_value.is_some()
                    {
                        return Err(format!(
                            "Transit feeder mode '{}' must configure both each filter attribute and its value",
                            setting.mode
                        ));
                    }
                }
                let utilities = config
                    .scoring()
                    .mode_params
                    .iter()
                    .map(|params| (params.mode.clone(), params.marginal_utility_of_traveling))
                    .collect::<BTreeMap<_, _>>();
                transit_router = transit_router.with_intermodal_access_egress(
                    config.transit().intermodal_access_egress.clone(),
                    feeder_routers,
                    utilities,
                    config.transit().intermodal_access_egress_mode_selection,
                    config.transit().intermodal_leg_only_handling,
                    config.computational_setup().random_seed,
                );
            }
            routers.insert(mode, Arc::new(transit_router));
        }

        Ok(TripRouter::new(routers))
    }
}

impl Controller {
    /// Runs the simulation and joins all threads before returning.
    pub fn run(mut self) -> (TripRouter, Population) {
        let first_iteration = self.config.controller().first_iteration;
        let last_iteration = self.config.controller().last_iteration;
        assert!(
            first_iteration <= last_iteration,
            "Invalid simulation iteration range: first_iteration ({first_iteration}) must be less than or equal to last_iteration ({last_iteration})."
        );
        assert!(
            self.config.output().write_events == WriteEvents::None
                || self.config.controller().write_events_interval > 0,
            "Invalid controller config: write_events_interval must be greater than 0 when event writing is enabled."
        );
        assert!(
            self.config.controller().write_plans_interval > 0,
            "Invalid controller config: write_plans_interval must be greater than 0."
        );
        if self.config.output().analysis.enabled {
            assert!(
                self.config.output().analysis.interval_seconds > 0,
                "Invalid output.analysis.interval_seconds: must be greater than 0."
            );
            assert!(
                self.config.output().write_events == WriteEvents::File,
                "Automatic analysis requires output.write_events: File."
            );
            assert!(
                self.config.qsim().sample_size > 0.0,
                "Automatic analysis requires a positive qsim.sample_size: volumes are scaled up by its reciprocal."
            );
        }

        self.controller_events_manager
            .reset_iteration(first_iteration);
        self.controller_events_manager
            .process_event(ControllerEvent::startup(first_iteration == last_iteration));

        let output_path = io::resolve_path(self.config.context(), &self.config.output().output_dir);
        let iters_path = output_path.join("ITERS");

        prepare_output_directory(&output_path, self.config.output().overwrite_files)
            .unwrap_or_else(|err| panic!("{err}"));
        fs::create_dir_all(&iters_path).expect("Failed to create iters output path");

        if Logging::Info == self.config.output().logging {
            let log_path = output_path.join("logs");
            fs::create_dir_all(&log_path).expect("Failed to create logs output path");
        }

        let _controller_log_guards = init_controller_logging(&self.config);
        let simulation_started = Instant::now();
        let mut mobsim_workers = self.start_mobsim_workers();
        let scoring_pool = ScoringPool::new(&self.scenario.core, self.scoring_function.take());
        let replanning_pool = ReplanningPool::new(
            &self.scenario.core,
            self.trip_router.clone(),
            mem::take(&mut self.replanning_strategies),
        );
        let mut phase_seconds = BTreeMap::new();

        for iteration in first_iteration..=last_iteration {
            if iteration != first_iteration {
                self.controller_events_manager.reset_iteration(iteration);
            }
            self.run_iteration(
                iteration,
                last_iteration,
                &mut mobsim_workers,
                &scoring_pool,
                &replanning_pool,
                &iters_path,
                &mut phase_seconds,
            );
        }

        let finalization_started = Instant::now();
        mobsim_workers.shutdown();
        self.shutdown_adapters();

        info!("Writing output files:");
        if self.config.controller().compression_type.is_protobuf() {
            info!("    ... ID store ...");
            Self::write_output_id_store(&output_path);
        }
        info!("    ... Config ...");
        self.write_output_config(output_path.clone());
        info!("    ... Network ...");
        self.write_output_network(output_path.clone());
        info!("    ... Population ...");
        self.write_output_population(output_path.clone());

        if self.config.output().write_events == WriteEvents::File {
            info!("Copying events to main output directory");
            self.copy_events_file(output_path.clone(), last_iteration);
        }

        self.controller_events_manager
            .process_event(ControllerEvent::shutdown(true));
        phase_seconds.insert(
            "shutdown_and_output".to_owned(),
            finalization_started.elapsed().as_secs_f64(),
        );

        if self.config.output().analysis.enabled {
            // The output network written above is the eligible-link set a standalone rerun reads.
            let network_file = self
                .config
                .controller()
                .compression_type
                .with_extension("output_network");
            let metadata = crate::simulation::analysis::AnalysisRunMetadata::from_run(
                self.config.computational_setup().random_seed,
                // The report scales observed volumes up by the reciprocal of this.
                self.config.qsim().sample_size,
                // The event files only hold events from the window onwards, so the report
                // cannot recover when the window opened.
                self.config.qsim().start_time,
                &self.scenario.core.garage,
                // The run has finished, so the snapshot moves into the report instead of copied.
                std::mem::take(&mut self.expected_travel),
                crate::simulation::analysis::AnalysisInputPaths {
                    network: self.config.network().path.as_deref(),
                    network_file: Some(Path::new(&network_file)),
                    population: self.config.population().path.as_deref(),
                    vehicles: self.config.vehicles().path.as_deref(),
                },
            )
            .with_person_demographics(std::mem::take(&mut self.person_demographics))
            .with_transit(
                &self.scenario.core.transit_schedule,
                &self.scenario.core.garage,
            )
            .with_runtime(crate::simulation::analysis::AnalysisRuntimeMetadata {
                simulation_seconds: Some(simulation_started.elapsed().as_secs_f64()),
                phase_seconds,
                worker_count: Some(self.config.partitioning().num_parts as usize),
                available_logical_cpus: std::thread::available_parallelism().ok().map(usize::from),
                operating_system: Some(std::env::consts::OS.to_owned()),
                architecture: Some(std::env::consts::ARCH.to_owned()),
                cpu_model: crate::simulation::analysis::host_cpu_model(),
                host_memory_bytes: crate::simulation::analysis::host_memory_bytes(),
                software_name: Some(env!("CARGO_PKG_NAME").to_owned()),
                software_version: Some(env!("CARGO_PKG_VERSION").to_owned()),
                network_links: Some(self.scenario.core.network.links().len()),
                population_persons: Some(self.scenario.population.persons.len()),
                vehicles: Some(self.scenario.core.garage.vehicles.len()),
                peak_memory_bytes: crate::simulation::analysis::process_peak_memory_bytes(),
                ..crate::simulation::analysis::AnalysisRuntimeMetadata::default()
            });
            let report = crate::simulation::analysis::analyze_final_iteration(
                &output_path,
                last_iteration,
                self.config.partitioning().num_parts,
                self.config.controller().compression_type,
                self.config.qsim().end_time,
                &metadata,
                &self.scenario.core.network,
                &self.config.output().analysis,
            )
            .unwrap_or_else(|err| panic!("Automatic analysis failed: {err}"));
            info!("Analysis report: {}", report.display());
        }
        (self.trip_router, self.scenario.population)
    }

    fn run_iteration(
        &mut self,
        iteration: u32,
        end_iter: u32,
        mobsim_workers: &mut MobsimWorkerPool,
        scoring_pool: &ScoringPool,
        replanning_pool: &ReplanningPool,
        iters_path: impl AsRef<Path>,
        phase_seconds: &mut BTreeMap<String, f64>,
    ) {
        let is_last_iteration = iteration == end_iter;
        info!("=========== Start Iteration {} ===========", iteration);

        self.controller_events_manager
            .process_event(ControllerEvent::iteration_starts(is_last_iteration));

        let started = Instant::now();
        let population = self.run_mobsim_phase(iteration, is_last_iteration, mobsim_workers);
        add_phase_time(phase_seconds, "mobsim", started.elapsed());
        let started = Instant::now();
        let population =
            self.run_scoring_phase(iteration, is_last_iteration, scoring_pool, population);
        add_phase_time(phase_seconds, "scoring", started.elapsed());

        if self
            .config
            .controller()
            .should_write_plans(iteration, is_last_iteration)
        {
            let started = Instant::now();
            self.write_iteration_files(iteration, iters_path, &population);
            add_phase_time(phase_seconds, "iteration_output", started.elapsed());
        }

        let population = if is_last_iteration {
            population
        } else {
            let started = Instant::now();
            let population = self.run_replanning_phase(iteration, replanning_pool, population);
            add_phase_time(phase_seconds, "replanning", started.elapsed());
            population
        };

        self.scenario.replace_population(population);

        self.controller_events_manager
            .process_event(ControllerEvent::iteration_ends(is_last_iteration));

        info!("=========== End Iteration {} ===========", iteration);
    }

    fn run_mobsim_phase(
        &mut self,
        iteration: u32,
        is_last_iteration: bool,
        mobsim_workers: &mut MobsimWorkerPool,
    ) -> Population {
        info!("Starting mobsim phase for iteration {iteration}");

        self.controller_events_manager
            .process_event(ControllerEvent::before_mobsim(is_last_iteration));

        prepare_for_mobsim(&mut self.scenario, &self.trip_router)
            .unwrap_or_else(|err| panic!("{err}: {:?}", err.issues()));
        if is_last_iteration && self.config.output().analysis.enabled {
            self.expected_travel =
                crate::simulation::analysis::capture_expected_travel(&self.scenario.population);
            // The grouping attributes are read here for the same reason: this is the last moment
            // the population still holds the attributes the run supplied.
            self.person_demographics = crate::simulation::analysis::capture_person_demographics(
                &self.scenario.population,
                &self.config.output().analysis,
            );
        }
        let inputs = self
            .scenario
            .split_for_mobsim(&self.link_storage_capacities);
        let agents = mobsim_workers.run_mobsim(iteration, is_last_iteration, inputs);

        self.controller_events_manager
            .process_event(ControllerEvent::after_mobsim(is_last_iteration));

        Population::from_agents(agents)
    }

    fn run_scoring_phase(
        &mut self,
        iteration: u32,
        is_last_iteration: bool,
        scoring_pool: &ScoringPool,
        mut population: Population,
    ) -> Population {
        info!("Starting scoring phase for iteration {iteration}");

        self.controller_events_manager
            .process_event(ControllerEvent::scoring(is_last_iteration));

        let mut experienced_plans = self.experienced_plan_collection.take(iteration);
        assert_eq!(
            population.persons.len(),
            experienced_plans.iter().map(|e| e.len()).sum::<usize>(),
            "Experienced population size differs from the mobsim population in iteration {iteration}."
        );

        info!(
            "Scoring population of {} persons in iteration {iteration}",
            population.persons.len()
        );
        scoring_pool.score_population(&mut experienced_plans, &mut population);
        info!("Finished scoring population in iteration {iteration}");

        if self.config.scoring().write_experienced_plans
            && self
                .config
                .controller()
                .should_write_plans(iteration, is_last_iteration)
        {
            let output_path =
                io::resolve_path(self.config.context(), &self.config.output().output_dir);
            write_experienced_population(
                experienced_plans,
                &population,
                &self.config,
                &output_path,
                iteration,
                is_last_iteration,
            );
        }

        population
    }

    fn run_replanning_phase(
        &mut self,
        iteration: u32,
        replanning_pool: &ReplanningPool,
        population: Population,
    ) -> Population {
        info!("Starting replanning phase for iteration {iteration}");

        self.controller_events_manager
            .process_event(ControllerEvent::replanning(false));

        let res = replanning_pool.replan(
            population,
            iteration,
            self.config.computational_setup().random_seed,
        );
        info!("Ending replanning phase for iteration {iteration}");
        res
    }

    fn start_mobsim_workers(&mut self) -> MobsimWorkerPool {
        let args = MobsimWorkerPoolArgumentsBuilder::default()
            .scenario_core(self.scenario.core.clone())
            .agent_source(self.agent_source.clone())
            .external_services(self.external_services.clone())
            .worker_listener(mem::take(&mut self.worker_listener))
            .global_barrier(self.global_barrier.clone())
            .build()
            .unwrap();

        MobsimWorkerPool::spawn(args)
    }

    fn shutdown_adapters(&mut self) {
        for adapter in mem::take(&mut self.adapter_handles) {
            adapter.shutdown_sender.send(true).unwrap();
            let name = adapter
                .handle
                .thread()
                .name()
                .unwrap_or("unnamed_thread")
                .to_string();
            adapter
                .handle
                .join()
                .unwrap_or_else(|_| panic!("Error in adapter thread {:?}", name));
        }
    }

    fn write_output_config(&mut self, output_path: PathBuf) {
        write_config(self.config.as_ref(), output_path);
    }

    fn write_output_network(&mut self, output_path: PathBuf) {
        let net_out_path = create_output_filename(
            &output_path,
            PathBuf::from(
                self.config
                    .controller()
                    .compression_type
                    .with_extension("output_network"),
            ),
        );

        let attribute_overrides = self.link_storage_capacities.attribute_overrides();
        self.scenario
            .core
            .network
            .to_file_with_link_attribute_overrides(&net_out_path, &attribute_overrides);
    }

    fn write_output_population(&mut self, output_path: impl AsRef<Path>) {
        let pop_out_path = create_output_filename(
            &output_path,
            PathBuf::from(
                self.config
                    .controller()
                    .compression_type
                    .with_extension("output_plans"),
            ),
        );

        self.scenario.population.to_file(&pop_out_path);
    }

    fn copy_events_file(&mut self, output_path: impl AsRef<Path>, last_iteration: u32) {
        let events_folder = output_path
            .as_ref()
            .join("ITERS")
            .join(format!("it.{}", last_iteration))
            .join("events");

        let options = CopyOptions::new().overwrite(true);
        fs_extra::dir::copy(&events_folder, output_path.as_ref(), &options).unwrap_or_else(
            |error| {
                panic!(
                    "Failed to copy events folder from {} to {}: {error}",
                    events_folder.display(),
                    output_path.as_ref().display()
                )
            },
        );
    }

    fn write_output_id_store(output_path: impl AsRef<Path>) {
        id::store_to_file(&output_path.as_ref().join(id::OUTPUT_FILE_NAME));
    }

    fn write_iteration_files(
        &self,
        iteration: u32,
        iters_path: impl AsRef<Path>,
        population: &Population,
    ) {
        let iter_path = iters_path.as_ref().join(format!("it.{}", iteration));
        population.to_file(
            &iter_path.join(
                self.config
                    .controller()
                    .compression_type
                    .with_extension("output_plans"),
            ),
        );
    }
}

fn add_phase_time(phases: &mut BTreeMap<String, f64>, phase: &str, elapsed: Duration) {
    *phases.entry(phase.to_owned()).or_default() += elapsed.as_secs_f64();
}

fn prepare_output_directory(
    output_path: &Path,
    overwrite_files: OverwriteFiles,
) -> Result<(), String> {
    if output_path.exists() {
        match overwrite_files {
            OverwriteFiles::DeleteDirectoryIfExists => {
                fs::remove_dir_all(output_path).map_err(|err| {
                    format!(
                        "Failed to delete existing output directory {}: {}",
                        output_path.display(),
                        err
                    )
                })?
            }
            OverwriteFiles::FailIfDirectoryExists => {
                return Err(format!(
                    "Output directory already exists: {}",
                    output_path.display()
                ));
            }
            OverwriteFiles::OverwriteExistingFiles => {}
        }
    }

    fs::create_dir_all(output_path).map_err(|err| {
        format!(
            "Failed to create output path {}: {}",
            output_path.display(),
            err
        )
    })?;

    Ok(())
}

pub(crate) fn write_experienced_population(
    experienced_plans: Vec<PersonExperiences>,
    original_population: &Population,
    config: &Config,
    output_path: &Path,
    iteration: u32,
    is_last_iteration: bool,
) {
    let persons: Vec<_> = experienced_plans
        .into_iter()
        .flat_map(|e| e.into_iter())
        .map(|(person_id, experience)| {
            let original = original_population
                .persons
                .get(&person_id)
                .unwrap_or_else(|| {
                    panic!(
                        "No original person {} is available for experienced-plan output.",
                        person_id.external()
                    )
                });
            experience.convert_to_person(original)
        })
        .sorted_by(|a, b| a.id().cmp(b.id()))
        .collect();
    let filename = config
        .controller()
        .compression_type
        .with_extension("output_experienced_plans");
    let iteration_path = output_path
        .join("ITERS")
        .join(format!("it.{iteration}"))
        .join(&filename);
    info!("Writing experienced plans to {}", iteration_path.display());
    let population = Population::from_persons(persons);
    population.to_file(&iteration_path);

    if is_last_iteration {
        let root_path = output_path.join(filename);
        info!("Writing experienced plans to {}", root_path.display());
        population.to_file(&root_path);
    }
}

#[cfg(test)]
mod tests {
    use super::prepare_output_directory;
    use crate::simulation::config::OverwriteFiles;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn delete_directory_if_exists_recreates_output_dir() {
        let dir = tempdir().unwrap();
        let output_dir = dir.path().join("output");
        fs::create_dir_all(&output_dir).unwrap();
        let stale_file = output_dir.join("stale.txt");
        fs::write(&stale_file, "stale").unwrap();

        prepare_output_directory(&output_dir, OverwriteFiles::DeleteDirectoryIfExists).unwrap();

        assert!(output_dir.exists());
        assert!(!stale_file.exists());
    }

    #[test]
    fn fail_if_directory_exists_returns_error() {
        let dir = tempdir().unwrap();
        let output_dir = dir.path().join("output");
        fs::create_dir_all(&output_dir).unwrap();

        let result = prepare_output_directory(&output_dir, OverwriteFiles::FailIfDirectoryExists);

        assert!(result.is_err());
    }

    #[test]
    fn overwrite_existing_files_keeps_existing_directory_contents() {
        let dir = tempdir().unwrap();
        let output_dir = dir.path().join("output");
        fs::create_dir_all(&output_dir).unwrap();
        let existing_file = output_dir.join("existing.txt");
        fs::write(&existing_file, "keep").unwrap();

        prepare_output_directory(&output_dir, OverwriteFiles::OverwriteExistingFiles).unwrap();

        assert!(output_dir.exists());
        assert!(existing_file.exists());
    }
}
