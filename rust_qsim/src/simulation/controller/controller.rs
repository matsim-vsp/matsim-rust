use crate::external_services::AdapterHandle;
use crate::simulation::config::{Config, Logging, OverwriteFiles, WriteEvents, write_config};
use crate::simulation::controller::{
    ExternalServices, MobsimWorkerPool, MobsimWorkerPoolArgumentsBuilder, ReplanningPool,
    create_output_filename,
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
use crate::simulation::replanning::routing::a_star::{AStar, AltHeuristic};
use crate::simulation::replanning::routing::cost::ScoringBasedTravelTimeAndDisutility;
use crate::simulation::replanning::routing::network_routing::NetworkRoutingModule;
use crate::simulation::replanning::routing::teleportation::TeleportationRoutingModule;
use crate::simulation::replanning::routing::travel_time_calculator::{
    GlobalTravelTimeCalculator, PartitionTravelTimeCollector,
};
use crate::simulation::replanning::routing::{RoutingModule, TransitRoutingModule, TripRouter};
use crate::simulation::scenario::population::Population;
use crate::simulation::scenario::prepare_for_sim::prepare_for_sim;
use crate::simulation::scenario::{ControllerScenario, Scenario};
use crate::simulation::{id, io};
use derive_more::Debug;
use fs_extra::dir::CopyOptions;
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
}

pub struct ControllerBuilder {
    scenario: Scenario,
    agent_source: DynAgentSource,
    controller_event_register_fn: Vec<Box<ControllerListenerRegisterFn>>,
    worker_listener_register_fn: HashMap<u32, Vec<Box<WorkerListenerRegisterFunction>>>,
    external_services: ExternalServices,
    global_barrier: Option<Arc<Barrier>>,
    adapter_handles: Vec<AdapterHandle>,
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
        }
    }

    // Implementing a custom build function in order to set the barrier if not set by the user.
    pub fn build(mut self) -> Result<Controller, String> {
        self.scenario.config.travel_time_calculator().validate()?;
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

        let link_storage_capacities = LinkStorageCapacities::from_network(
            &self.scenario.network,
            self.scenario.config.qsim(),
        );
        let scenario: ControllerScenario = self.scenario.into();
        let config = scenario.core.config.clone();

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
                .push(Box::new(move |events, mobsim, partition| {
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

        // for every main mode, create the corresponding router.
        for mode in &config.qsim().main_modes {
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
            routers.insert(
                mode,
                Arc::new(TransitRoutingModule::new(
                    controller_scenario.core.transit_schedule.clone(),
                    walk.teleported_mode_speed,
                    walk.beeline_distance_factor,
                    controller_scenario.core.garage.clone(),
                    car_fallback,
                )),
            );
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
        let replanning_pool = ReplanningPool::new(&self.scenario.core, self.trip_router.clone());
        let mut phase_seconds = BTreeMap::new();

        for iteration in first_iteration..=last_iteration {
            self.run_iteration(
                iteration,
                last_iteration,
                &mut mobsim_workers,
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
        let population = self.run_scoring_phase(iteration, is_last_iteration, population);
        add_phase_time(phase_seconds, "scoring", started.elapsed());

        if self.should_write_iteration_plans(iteration, is_last_iteration) {
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

        prepare_for_sim(&mut self.scenario, &self.trip_router)
            .unwrap_or_else(|err| panic!("{err}: {:?}", err.issues()));
        if is_last_iteration && self.config.output().analysis.enabled {
            self.expected_travel =
                crate::simulation::analysis::capture_expected_travel(&self.scenario.population);
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
        mut population: Population,
    ) -> Population {
        info!("Starting scoring phase for iteration {iteration}");

        self.controller_events_manager
            .process_event(ControllerEvent::scoring(is_last_iteration));

        // Dummy impl: set scores to 1.0 for all persons.
        population
            .persons
            .values_mut()
            .flat_map(|p| p.plans_mut())
            .for_each(|p| p.score = Some(1.0));

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

        replanning_pool.replan(
            population,
            iteration,
            self.config.computational_setup().random_seed,
        )
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

    fn should_write_iteration_plans(&self, iteration: u32, is_last_iteration: bool) -> bool {
        is_last_iteration
            || (iteration != 0
                && iteration.is_multiple_of(self.config.controller().write_plans_interval))
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
