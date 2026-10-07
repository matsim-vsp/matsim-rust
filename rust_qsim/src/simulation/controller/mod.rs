#[allow(clippy::module_inception)]
pub mod controller;

use crate::external_services::{ExternalServiceType, RequestToAdapter};
use crate::simulation::agents::agent::SimulationAgent;
use crate::simulation::config::{CompressionType, WriteEvents};
use crate::simulation::events::{EventHandlerRegisterFn, EventTrait, EventsManager};
use crate::simulation::framework_events::{
    MobsimEvent, MobsimEventsManager, PartitionEventsManager, WorkerListenerRegisterFunction,
};
use crate::simulation::io::proto::proto_events::ProtoEventsWriter;
use crate::simulation::io::xml::events::XmlEventsWriter;
use crate::simulation::messaging::partition_change::PartitionChangeExtensionsManager;
use crate::simulation::messaging::sim_communication::local_communicator::ChannelSimCommunicator;
use crate::simulation::messaging::sim_communication::message_broker::NetMessageBroker;
use crate::simulation::population::agent_source::DynAgentSource;
use crate::simulation::replanning::routing::TripRouter;
use crate::simulation::replanning::{ReplanningStrategy, StrategyManager, replan_population};
use crate::simulation::scenario::population::Population;
use crate::simulation::scenario::{MobsimInput, ScenarioCore};
use crate::simulation::scoring::{
    CharyparNagelScoringFunction, PersonExperiences, PlanScorer, score_population,
};
use crate::simulation::simulation::{Simulation, SimulationBuilder};
use crate::simulation::{io, logging};
use derive_builder::Builder;
use derive_more::Debug;
use nohash_hasher::IntMap;
use std::any::Any;
use std::cell::{RefCell, RefMut};
use std::collections::HashMap;
use std::fs;
use std::mem;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver as StdReceiver, Sender as StdSender};
use std::sync::{Arc, Barrier};
use std::thread::JoinHandle;
use tokio::sync::mpsc::Sender;
use tracing::info;

// This is a wrapper around a Sender that can be used to send requests to an external service.
// The value is of type Arc as this is the adapter running in another thread.
// It needs to be super generic to store any type of Sender.
#[derive(Clone, Debug)]
pub struct RequestSender(Arc<dyn Any + Send + Sync>);

// Implementing this trait provides type safety for the RequestSender. Only Senders can be converted to RequestSender.
// This comes with the requirement that the type T must 'static in particular -- but I think this is ok for now. Paul, jul'25
impl<T> From<Arc<Sender<T>>> for RequestSender
where
    T: RequestToAdapter + 'static,
{
    fn from(value: Arc<Sender<T>>) -> Self {
        RequestSender(value as Arc<dyn Any + Send + Sync>)
    }
}

impl<T> From<Sender<T>> for RequestSender
where
    T: Send + 'static,
{
    fn from(value: Sender<T>) -> Self {
        RequestSender(Arc::new(value) as Arc<dyn Any + Send + Sync>)
    }
}

/// Holds a map of external services that can be used in the simulation.
#[derive(Debug, Default, Clone)]
pub struct ExternalServices(HashMap<ExternalServiceType, RequestSender>);

impl From<HashMap<ExternalServiceType, RequestSender>> for ExternalServices {
    fn from(value: HashMap<ExternalServiceType, RequestSender>) -> Self {
        ExternalServices(value)
    }
}

impl ExternalServices {
    pub fn get_service<T: Any + Send + Sync>(
        &self,
        service_type: ExternalServiceType,
    ) -> Option<&T> {
        self.0
            .get(&service_type)
            .and_then(|s| s.0.downcast_ref::<T>())
    }

    pub fn insert(&mut self, service_type: ExternalServiceType, sender: RequestSender) {
        self.0.insert(service_type, sender);
    }
}

/// This struct holds objects that are local to a thread running a simulation partition.
/// They function as a connector between the simulation partition and the "outside" computational context.
#[derive(Clone, Debug, Builder)]
#[builder(pattern = "owned")]
pub struct ThreadLocalComputationalEnvironment {
    #[builder(default)]
    services: ExternalServices,
    // The value is of type Rc as this is a thread-local events manager.
    #[builder(default)]
    events_manager: Rc<RefCell<EventsManager>>,
    mobsim_events_manager: Rc<RefCell<MobsimEventsManager>>,
    partition_events_manager: Rc<RefCell<PartitionEventsManager>>,
    #[builder(default)]
    partition_migration_extensions_manager: Rc<RefCell<PartitionChangeExtensionsManager>>,
}

#[cfg(test)]
impl Default for ThreadLocalComputationalEnvironment {
    fn default() -> Self {
        ThreadLocalComputationalEnvironment {
            services: ExternalServices::default(),
            events_manager: Rc::new(RefCell::new(EventsManager::new())),
            mobsim_events_manager: Rc::new(RefCell::new(MobsimEventsManager::default())),
            partition_events_manager: Rc::new(RefCell::new(PartitionEventsManager::default())),
            partition_migration_extensions_manager: Rc::new(RefCell::new(
                PartitionChangeExtensionsManager::default(),
            )),
        }
    }
}

impl ThreadLocalComputationalEnvironment {
    pub fn get_service<T: Any + Send + Sync>(
        &self,
        service_type: ExternalServiceType,
    ) -> Option<&T> {
        self.services.get_service(service_type)
    }

    pub fn events_manager_borrow_mut(&mut self) -> RefMut<'_, EventsManager> {
        self.events_manager.borrow_mut()
    }

    pub fn events_manager(&self) -> Rc<RefCell<EventsManager>> {
        self.events_manager.clone()
    }

    pub fn mobsim_events_manager_borrow_mut(&mut self) -> RefMut<'_, MobsimEventsManager> {
        self.mobsim_events_manager.borrow_mut()
    }

    pub fn mobsim_event_bus(&self) -> Rc<RefCell<MobsimEventsManager>> {
        self.mobsim_events_manager.clone()
    }

    pub fn partition_events_manager_borrow_mut(&mut self) -> RefMut<'_, PartitionEventsManager> {
        self.partition_events_manager.borrow_mut()
    }

    pub fn partition_event_bus(&self) -> Rc<RefCell<PartitionEventsManager>> {
        self.partition_events_manager.clone()
    }

    pub fn partition_migration_extensions_manager_borrow_mut(
        &mut self,
    ) -> RefMut<'_, PartitionChangeExtensionsManager> {
        self.partition_migration_extensions_manager.borrow_mut()
    }

    pub fn reset_iteration(&mut self, iteration: u32) {
        self.events_manager.borrow_mut().reset_iteration(iteration);
        self.mobsim_events_manager
            .borrow_mut()
            .reset_iteration(iteration);
        self.partition_events_manager
            .borrow_mut()
            .reset_iteration(iteration);
    }

    pub fn finish_events(&mut self) {
        self.events_manager.borrow_mut().finish();
    }
}

pub(crate) struct MobsimWorkerPool {
    command_senders: IntMap<u32, StdSender<MobsimWorkerCommand>>,
    result_receiver: StdReceiver<MobsimWorkerResult>,
    handles: IntMap<u32, JoinHandle<()>>,
    num_parts: u32,
}

pub(crate) enum MobsimWorkerCommand {
    RunMobsim(Box<MobsimWorkerRun>),
    Shutdown,
}

pub(crate) struct MobsimWorkerResult {
    pub rank: u32,
    pub iteration: u32,
    pub agents: Vec<SimulationAgent>,
}

pub(crate) struct MobsimWorkerRun {
    iteration: u32,
    is_last_iteration: bool,
    input: MobsimInput,
}

#[derive(Builder)]
#[builder(pattern = "owned")]
pub(crate) struct MobsimWorkerPoolArguments {
    scenario_core: ScenarioCore,
    agent_source: DynAgentSource,
    #[builder(default)]
    external_services: ExternalServices,
    #[builder(default)]
    worker_listener: HashMap<u32, Vec<Box<WorkerListenerRegisterFunction>>>,
    global_barrier: Arc<Barrier>,
}

#[derive(Builder)]
#[builder(pattern = "owned")]
struct MobsimWorkerArguments {
    rank: u32,
    communicator: ChannelSimCommunicator,
    scenario_core: ScenarioCore,
    agent_source: DynAgentSource,
    #[builder(default)]
    external_services: ExternalServices,
    #[builder(default)]
    worker_listener: Vec<Box<WorkerListenerRegisterFunction>>,
    global_barrier: Arc<Barrier>,
}

struct MobsimWorker {
    rank: u32,
    // the current implementation requires a new worker every iteration. Thus, the worker holds a reference to the communicator, but does not own it.
    communicator: Rc<ChannelSimCommunicator>,
    scenario_core: ScenarioCore,
    agent_source: DynAgentSource,
    comp_env: ThreadLocalComputationalEnvironment,
    global_barrier: Arc<Barrier>,
    reached_initial_barrier: bool,
}

impl MobsimWorkerPool {
    pub(crate) fn spawn(mut args: MobsimWorkerPoolArguments) -> Self {
        let num_parts = args.scenario_core.config.partitioning().num_parts;
        let comms = ChannelSimCommunicator::create_n_2_n(num_parts);
        let (result_sender, result_receiver) = mpsc::channel();
        let mut command_senders = IntMap::default();
        let mut handles = IntMap::default();

        // MobSim workers are long-lived, partition-affine, and can block in communicator
        // synchronization. Keep them on dedicated threads; Replanning uses Rayon separately.
        for comm in comms {
            let rank = comm.rank();
            let (command_sender, command_receiver) = mpsc::channel();
            let worker_result_sender = result_sender.clone();
            let worker_args = MobsimWorkerArgumentsBuilder::default()
                .rank(rank)
                .communicator(comm)
                .scenario_core(args.scenario_core.clone())
                .agent_source(args.agent_source.clone())
                .external_services(args.external_services.clone())
                .worker_listener(args.worker_listener.remove(&rank).unwrap_or_default())
                .global_barrier(args.global_barrier.clone())
                .build()
                .unwrap();

            let handle = std::thread::Builder::new()
                .name(format!("qsim-{rank}"))
                .spawn(move || {
                    run_mobsim_worker(worker_args, command_receiver, worker_result_sender)
                })
                .unwrap();

            command_senders.insert(rank, command_sender);
            handles.insert(rank, handle);
        }

        Self {
            command_senders,
            result_receiver,
            handles,
            num_parts,
        }
    }

    pub(crate) fn run_mobsim(
        &mut self,
        iteration: u32,
        is_last_iteration: bool,
        inputs: Vec<MobsimInput>,
    ) -> Vec<SimulationAgent> {
        assert_eq!(
            inputs.len(),
            self.num_parts as usize,
            "Expected one mobsim input per mobsim worker."
        );

        // send commands to worker threads
        for input in inputs {
            let rank = input.partition.rank;
            self.command_senders
                .get(&rank)
                .unwrap_or_else(|| panic!("No mobsim worker command sender for rank {rank}."))
                .send(MobsimWorkerCommand::RunMobsim(Box::new(MobsimWorkerRun {
                    iteration,
                    is_last_iteration,
                    input,
                })))
                .unwrap_or_else(|err| {
                    panic!("Failed to send mobsim command to rank {rank}: {err}")
                });
        }

        // wait for mobsim to be finished and receive population
        let mut results: IntMap<u32, Vec<SimulationAgent>> = IntMap::default();
        for _ in 0..self.num_parts {
            let result = self
                .result_receiver
                .recv()
                .unwrap_or_else(|err| panic!("Failed to receive mobsim worker result: {err}"));
            assert_eq!(
                result.iteration, iteration,
                "Received mobsim result for iteration {}, expected {}.",
                result.iteration, iteration
            );
            let previous = results.insert(result.rank, result.agents);
            assert!(
                previous.is_none(),
                "Received duplicate mobsim result for rank {} in iteration {}.",
                result.rank,
                iteration
            );
        }

        let mut agents = Vec::new();
        for rank in 0..self.num_parts {
            agents.extend(results.remove(&rank).unwrap_or_else(|| {
                panic!("Missing mobsim result for rank {rank} in iteration {iteration}.")
            }));
        }
        agents
    }

    pub(crate) fn shutdown(self) {
        // send shutdown signal
        for (rank, sender) in &self.command_senders {
            sender
                .send(MobsimWorkerCommand::Shutdown)
                .unwrap_or_else(|err| {
                    panic!("Failed to send mobsim shutdown command to rank {rank}: {err}")
                });
        }

        // wait for the threads to finish
        for (rank, handle) in self.handles {
            handle
                .join()
                .unwrap_or_else(|_| panic!("Mobsim worker rank {rank} panicked."));
        }
    }
}

fn run_mobsim_worker(
    args: MobsimWorkerArguments,
    command_receiver: StdReceiver<MobsimWorkerCommand>,
    result_sender: StdSender<MobsimWorkerResult>,
) {
    let mut worker = MobsimWorker::new(args);
    let _guards = logging::init_logging(&worker.scenario_core.config, worker.rank);

    worker.run_loop(command_receiver, result_sender);

    drop(_guards);
}

impl MobsimWorker {
    fn new(args: MobsimWorkerArguments) -> Self {
        let MobsimWorkerArguments {
            rank,
            communicator,
            scenario_core,
            agent_source,
            external_services,
            mut worker_listener,
            global_barrier,
        } = args;

        let config = &scenario_core.config;
        let additional_subscribers = mem::take(&mut worker_listener);
        let output_path = io::resolve_path(config.context(), &config.output().output_dir);

        let mut events = EventsManager::new();
        let mut mobsim_events = MobsimEventsManager::for_partition(rank, 0);
        let mut partition_events = PartitionEventsManager::for_partition(rank, 0);
        let mut partition_changes = PartitionChangeExtensionsManager::new();

        if config.output().write_events != WriteEvents::None {
            assert!(
                config.controller().write_events_interval > 0,
                "Invalid controller config: write_events_interval must be greater than 0 when event writing is enabled."
            );
            IterationEventsWriter::register(
                output_path,
                rank,
                config.output().write_events.clone(),
                config.controller().compression_type,
                config.controller().write_events_interval,
                config.controller().last_iteration,
            )(&mut events);
        }

        for subscriber in additional_subscribers {
            subscriber(
                &mut events,
                &mut mobsim_events,
                &mut partition_events,
                &mut partition_changes,
            );
        }

        let comp_env = ThreadLocalComputationalEnvironmentBuilder::default()
            .services(external_services)
            .events_manager(Rc::new(RefCell::new(events)))
            .mobsim_events_manager(Rc::new(RefCell::new(mobsim_events)))
            .partition_events_manager(Rc::new(RefCell::new(partition_events)))
            .partition_migration_extensions_manager(Rc::new(RefCell::new(partition_changes)))
            .build()
            .unwrap();

        Self {
            rank,
            communicator: Rc::new(communicator),
            scenario_core,
            agent_source,
            comp_env,
            global_barrier,
            reached_initial_barrier: false,
        }
    }

    fn run_loop(
        &mut self,
        command_receiver: StdReceiver<MobsimWorkerCommand>,
        result_sender: StdSender<MobsimWorkerResult>,
    ) {
        while let Ok(command) = command_receiver.recv() {
            match command {
                MobsimWorkerCommand::RunMobsim(run) => {
                    let MobsimWorkerRun {
                        iteration,
                        is_last_iteration,
                        input,
                    } = *run;
                    info!(
                        "Mobsim worker #{} starting iteration {}. Last iteration: {}",
                        self.rank, iteration, is_last_iteration
                    );
                    let agents = self.run_iteration(iteration, input);
                    self.comp_env
                        .mobsim_events_manager_borrow_mut()
                        .process_event(MobsimEvent::BeforeCleanup);
                    result_sender
                        .send(MobsimWorkerResult {
                            rank: self.rank,
                            iteration,
                            agents,
                        })
                        .unwrap_or_else(|err| {
                            panic!(
                                "Mobsim worker rank {} failed to send result for iteration {}: {}",
                                self.rank, iteration, err
                            )
                        });
                }
                MobsimWorkerCommand::Shutdown => {
                    info!("Mobsim worker #{} shutting down.", self.rank);
                    break;
                }
            }
        }

        self.comp_env.finish_events();
    }

    fn run_iteration(&mut self, iteration: u32, input: MobsimInput) -> Vec<SimulationAgent> {
        self.comp_env.reset_iteration(iteration);
        assert_eq!(
            input.partition.rank, self.rank,
            "Mobsim worker rank {} received input for rank {}.",
            self.rank, input.partition.rank
        );

        let net_message_broker = NetMessageBroker::new(
            self.communicator.clone(),
            &input.partition.scenario.network,
            &input.partition.network_partition,
            input
                .partition
                .scenario
                .config
                .computational_setup()
                .global_sync,
        );

        // Create a new simulation for this worker each iteration. This makes sure that there is no state carried over from previous iterations, which could lead to bugs.
        let mut simulation: Simulation<ChannelSimCommunicator> = SimulationBuilder::new(
            input,
            net_message_broker,
            self.comp_env.clone(),
            self.agent_source.clone(),
        )
        .build();

        if !self.reached_initial_barrier {
            let size = self.scenario_core.config.partitioning().num_parts;
            info!(
                "Process #{} (0-indexed) of {} processes has arrived at initial barrier. Waiting for other processes and potential external services to reach global barrier.",
                self.rank, size
            );
            self.global_barrier.wait();
            self.reached_initial_barrier = true;
        }

        simulation.run()
    }
}

pub(crate) struct ReplanningPool {
    pool: Option<rayon::ThreadPool>,
    strategy_manager: StrategyManager,
    trip_router: TripRouter,
    first_iteration: u32,
    last_iteration: u32,
    innovation_disable_fraction: f64,
    batch_previous_route_proposals: bool,
}

pub(crate) struct ScoringPool {
    pool: Option<rayon::ThreadPool>,
    plan_scorer: Box<dyn PlanScorer>,
}

impl ReplanningPool {
    pub(crate) fn new(
        scenario_core: &ScenarioCore,
        trip_router: TripRouter,
        custom_strategies: Vec<Box<dyn ReplanningStrategy>>,
    ) -> Self {
        let config = scenario_core.config.as_ref();
        let threads = config.computational_setup().replanning_threads;
        let pool = if threads == 0 {
            None
        } else {
            Some(
                rayon::ThreadPoolBuilder::new()
                    .num_threads(threads as usize)
                    .thread_name(|i| format!("replanning-{i}"))
                    .build()
                    .expect("Failed to build replanning thread pool."),
            )
        };
        Self {
            pool,
            strategy_manager: StrategyManager::from_replanning_config(
                config.replanning(),
                trip_router.clone(),
                scenario_core,
                custom_strategies,
            ),
            trip_router,
            first_iteration: config.controller().first_iteration,
            last_iteration: config.controller().last_iteration,
            innovation_disable_fraction: config
                .replanning()
                .fraction_of_iterations_to_disable_innovation,
            batch_previous_route_proposals: config.replanning().batch_previous_route_proposals,
        }
    }

    pub(crate) fn replan(
        &self,
        population: Population,
        iteration: u32,
        base_seed: u64,
    ) -> Population {
        let innovation_disabled = self.innovation_disabled(iteration);
        if self.batch_previous_route_proposals {
            self.trip_router
                .prepare_previous_route_proposals(&population);
        }
        let dispatch = tracing::dispatcher::get_default(Clone::clone);
        match &self.pool {
            Some(pool) => pool.install(|| {
                replan_population(
                    population,
                    iteration,
                    base_seed,
                    &self.strategy_manager,
                    innovation_disabled,
                    &dispatch,
                )
            }),
            None => replan_population(
                population,
                iteration,
                base_seed,
                &self.strategy_manager,
                innovation_disabled,
                &dispatch,
            ),
        }
    }

    fn innovation_disabled(&self, iteration: u32) -> bool {
        let total_iterations = self.last_iteration.saturating_sub(self.first_iteration);
        let progress = if total_iterations == 0 {
            1.0
        } else {
            iteration.saturating_sub(self.first_iteration) as f64 / total_iterations as f64
        };
        progress >= self.innovation_disable_fraction
    }
}

impl ScoringPool {
    pub(crate) fn new(
        scenario_core: &ScenarioCore,
        plan_scorer: Option<Box<dyn PlanScorer>>,
    ) -> Self {
        let threads = scenario_core.config.computational_setup().scoring_threads;
        let pool = if threads == 0 {
            None
        } else {
            Some(
                rayon::ThreadPoolBuilder::new()
                    .num_threads(threads as usize)
                    .thread_name(|index| format!("scoring-{index}"))
                    .build()
                    .expect("Failed to build scoring thread pool."),
            )
        };
        Self {
            pool,
            plan_scorer: plan_scorer.unwrap_or_else(|| {
                Box::new(CharyparNagelScoringFunction::new(
                    scenario_core.config.as_ref(),
                    scenario_core.network.clone(),
                ))
            }),
        }
    }

    pub(crate) fn score_population(
        &self,
        experienced_plans: &mut Vec<PersonExperiences>,
        population: &mut Population,
    ) {
        let mut score =
            || score_population(experienced_plans, population, self.plan_scorer.as_ref());
        match &self.pool {
            Some(pool) => pool.install(score),
            None => score(),
        }
    }
}

enum ActiveIterationEventsWriter {
    Proto(ProtoEventsWriter),
    Xml(XmlEventsWriter),
}

impl ActiveIterationEventsWriter {
    fn on_any(&mut self, event: &dyn EventTrait) {
        match self {
            Self::Proto(writer) => writer.on_any(event),
            Self::Xml(writer) => writer.on_any(event),
        }
    }

    fn finish(&mut self) {
        match self {
            Self::Proto(writer) => writer.finish(),
            Self::Xml(writer) => writer.finish(),
        }
    }
}

struct IterationEventsWriter {
    output_path: PathBuf,
    rank: u32,
    write_events: WriteEvents,
    compression_type: CompressionType,
    write_events_interval: u32,
    last_iteration: u32,
    active_writer: RefCell<Option<ActiveIterationEventsWriter>>,
}

impl IterationEventsWriter {
    fn register(
        output_path: PathBuf,
        rank: u32,
        write_events: WriteEvents,
        compression_type: CompressionType,
        write_events_interval: u32,
        last_iteration: u32,
    ) -> Box<EventHandlerRegisterFn> {
        Box::new(move |events: &mut EventsManager| {
            let writer = Rc::new(Self {
                output_path,
                rank,
                write_events,
                compression_type,
                write_events_interval,
                last_iteration,
                active_writer: RefCell::new(None),
            });

            let reset_writer = writer.clone();
            events.on_reset_iteration(move |iteration| {
                reset_writer.reset_iteration(iteration);
            });

            let event_writer = writer.clone();
            events.on_any(move |event| {
                event_writer.on_any(event);
            });

            events.on_finish(move || {
                writer.finish();
            });
        })
    }

    fn reset_iteration(&self, iteration: u32) {
        self.finish();

        if !self.should_write(iteration) {
            return;
        }

        let events_dir = self
            .output_path
            .join("ITERS")
            .join(format!("it.{iteration}"))
            .join("events");
        fs::create_dir_all(&events_dir).expect("Failed to create iteration events output path");

        let writer = match &self.write_events {
            WriteEvents::None => return,
            WriteEvents::File => {
                let events_path = events_dir.join(format!(
                    "events.{}.{}",
                    self.rank,
                    self.compression_type.extension()
                ));
                info!("adding events writer with path: {events_path:?}");
                match self.compression_type {
                    CompressionType::Proto => {
                        ActiveIterationEventsWriter::Proto(ProtoEventsWriter::new(events_path))
                    }
                    CompressionType::None | CompressionType::Gz | CompressionType::Zst => {
                        ActiveIterationEventsWriter::Xml(XmlEventsWriter::new(events_path))
                    }
                }
            }
        };

        *self.active_writer.borrow_mut() = Some(writer);
    }

    fn should_write(&self, iteration: u32) -> bool {
        iteration == self.last_iteration || iteration.is_multiple_of(self.write_events_interval)
    }

    fn on_any(&self, event: &dyn EventTrait) {
        if let Some(writer) = self.active_writer.borrow_mut().as_mut() {
            writer.on_any(event);
        }
    }

    /// Takes the active writer and calls finish on it, if it exists. This is called when the iteration is reset or when the simulation is finished.
    fn finish(&self) {
        if let Some(mut writer) = self.active_writer.borrow_mut().take() {
            writer.finish();
        }
    }
}

pub fn get_numbered_output_filename(
    output_dir: impl AsRef<Path>,
    input_file: impl AsRef<Path>,
    part: u32,
) -> PathBuf {
    let out = create_output_filename(output_dir, input_file);
    insert_number_in_proto_filename(&out, part)
}

pub fn create_output_filename(
    output_dir: impl AsRef<Path>,
    input_file: impl AsRef<Path>,
) -> PathBuf {
    let filename = input_file.as_ref().file_name().unwrap();
    output_dir.as_ref().join(filename)
}

pub(crate) fn insert_number_in_proto_filename(path: impl AsRef<Path>, part: u32) -> PathBuf {
    let filename = path.as_ref().file_name().unwrap().to_str().unwrap();

    let (stripped, ext) = if filename.ends_with(".xml.zst") {
        (filename.strip_suffix(".xml.zst").unwrap(), "xml.zst")
    } else if filename.ends_with(".xml.gz") {
        (filename.strip_suffix(".xml.gz").unwrap(), "xml.gz")
    } else if filename.ends_with(".xml") {
        (filename.strip_suffix(".xml").unwrap(), "xml")
    } else if filename.ends_with(".binpb") {
        (filename.strip_suffix(".binpb").unwrap(), "binpb")
    } else {
        panic!("Unknown file extension")
    };

    let stripped = stripped
        .strip_suffix(format!(".{part}").as_str())
        .unwrap_or(stripped);

    let new_filename = format!("{stripped}.{part}.{ext}");
    path.as_ref().parent().unwrap().join(new_filename)
}

#[cfg(test)]
mod tests {
    use super::{MobsimWorkerPool, MobsimWorkerPoolArgumentsBuilder, ReplanningPool};
    use crate::simulation::config::Config;
    use crate::simulation::framework_events::{MobsimEvent, WorkerListenerRegisterFunction};
    use crate::simulation::id::Id;
    use crate::simulation::network::signals::Signals;
    use crate::simulation::network::sim_network::SimNetworkPartition;
    use crate::simulation::population::agent_source::PopulationAgentSource;
    use crate::simulation::replanning::routing::TripRouter;
    use crate::simulation::scenario::network::Network;
    use crate::simulation::scenario::population::{InternalPerson, InternalPlan, Population};
    use crate::simulation::scenario::vehicles::Garage;
    use crate::simulation::scenario::{
        MobsimInput, MobsimScenarioPartition, PopulationShard, ScenarioCore,
    };
    use macros::deterministic_id_test;
    use nohash_hasher::IntSet;
    use std::collections::HashMap;
    use std::sync::{Arc, Barrier, Mutex};

    #[deterministic_id_test]
    fn mobsim_worker_pool_runs_empty_population_and_shuts_down() {
        let mut config = Config::default();
        config.qsim_mut().end_time = 0;
        let config = Arc::new(config);
        let scenario_core = ScenarioCore {
            network: Arc::new(Network::new()),
            garage: Arc::new(Garage::default()),
            transit_schedule: Arc::new(
                crate::simulation::scenario::transit::TransitSchedule::default(),
            ),
            transit_runs: Default::default(),
            config: config.clone(),
            signals: Arc::new(Signals::default()),
        };

        let completed_iterations = Arc::new(Mutex::new(Vec::new()));
        let completed_for_registration = completed_iterations.clone();
        let completion_listener: Box<WorkerListenerRegisterFunction> =
            Box::new(move |_, mobsim, _, _| {
                mobsim.on_event(move |event| {
                    if matches!(&event.payload, MobsimEvent::BeforeCleanup) {
                        completed_for_registration
                            .lock()
                            .unwrap()
                            .push(event.meta.iteration);
                    }
                });
            });
        let args = MobsimWorkerPoolArgumentsBuilder::default()
            .scenario_core(scenario_core.clone())
            .agent_source(Arc::new(PopulationAgentSource))
            .worker_listener(HashMap::from([(0, vec![completion_listener])]))
            .global_barrier(Arc::new(Barrier::new(1)))
            .build()
            .unwrap();
        let mut pool = MobsimWorkerPool::spawn(args);

        let agents = pool.run_mobsim(0, false, vec![empty_mobsim_input(&scenario_core)]);
        assert!(agents.is_empty());
        assert_eq!(&[0], completed_iterations.lock().unwrap().as_slice());

        let agents = pool.run_mobsim(1, true, vec![empty_mobsim_input(&scenario_core)]);
        assert!(agents.is_empty());
        assert_eq!(&[0, 1], completed_iterations.lock().unwrap().as_slice());

        pool.shutdown();
    }

    #[deterministic_id_test]
    fn replanning_pool_noop_preserves_person_ids() {
        let mut config = Config::default();
        config.computational_setup_mut().replanning_threads = 2;
        let scenario_core = ScenarioCore {
            network: Arc::new(Network::new()),
            garage: Arc::new(Garage::default()),
            transit_schedule: Arc::new(
                crate::simulation::scenario::transit::TransitSchedule::default(),
            ),
            transit_runs: Default::default(),
            config: Arc::new(config),
            signals: Arc::new(Signals::default()),
        };
        let pool = ReplanningPool::new(&scenario_core, TripRouter::default(), Vec::new());

        let population = Population::from_persons(vec![
            person("replanning-pool-person-1"),
            person("replanning-pool-person-2"),
        ]);
        let expected_ids: IntSet<_> = population.persons.keys().cloned().collect();

        let replanned = pool.replan(
            population,
            0,
            scenario_core.config.computational_setup().random_seed,
        );
        let actual_ids: IntSet<_> = replanned.persons.keys().cloned().collect();

        assert_eq!(expected_ids, actual_ids);
    }

    fn person(id: &str) -> InternalPerson {
        InternalPerson::new(Id::create(id), InternalPlan::default())
    }

    fn empty_mobsim_input(scenario: &ScenarioCore) -> MobsimInput {
        let network_partition =
            SimNetworkPartition::from_network_for_test(&scenario.network, 0, &scenario.config);

        MobsimInput {
            partition: MobsimScenarioPartition {
                rank: 0,
                scenario: scenario.clone(),
                network_partition,
            },
            population: PopulationShard {
                population: Population::new(),
            },
        }
    }
}
