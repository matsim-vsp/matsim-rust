use crate::simulation::build_info::GIT_VERSION;
use crate::simulation::config::VertexWeight::InLinkCapacity;
use crate::simulation::io::is_url;
use crate::simulation::replanning::{KEEP_LAST_SELECTED_STRATEGY_NAME, WORST_SCORE_STRATEGY_NAME};
use ahash::HashMap;
use clap::{Parser, ValueEnum};
use derive_builder::Builder;
use dyn_clone::DynClone;
#[cfg(feature = "http")]
use reqwest::Url;
use serde::{Deserialize, Deserializer, Serialize};
use std::any::Any;
use std::fmt::Debug;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tracing::{Level, info, warn};

pub const DEFAULT_RANDOM_SEED: u64 = 4711;

/// Macro to register an override handler for a specific config key
#[macro_export]
macro_rules! register_override {
    ($key:literal, $func:expr_2021) => {
        inventory::submit! {
            $crate::simulation::config::OverrideHandler {
                key: $key,
                apply: $func,
            }
        }
    };
}

struct OverrideHandler {
    key: &'static str,
    apply: fn(config: &mut Config, value: &str),
}

// Collect all OverrideHandler submitted from anywhere in the crate
inventory::collect!(OverrideHandler);

#[derive(Parser, Debug, Clone)]
#[command(author, version = GIT_VERSION, about, long_about = None)]
pub struct CommandLineArgs {
    #[arg(long, short)]
    pub config: String,
    #[arg(long= "set", value_parser = parse_key_val)]
    pub overrides: Vec<(String, String)>,
}

impl CommandLineArgs {
    pub fn new_with_path(path: impl ToString) -> Self {
        CommandLineArgs {
            config: path.to_string(),
            overrides: Vec::new(),
        }
    }
}

fn parse_key_val(s: &str) -> Result<(String, String), String> {
    let pos = s.find('=');
    match pos {
        Some(pos) => Ok((s[..pos].to_string(), s[pos + 1..].to_string())),
        None => Err(format!("invalid KEY=VALUE: no `=` found in `{}`", s)),
    }
}

#[derive(Serialize, Debug)]
pub struct Config {
    modules: HashMap<String, Box<dyn ConfigModule>>,
    #[serde(skip)]
    context: Option<PathBuf>,
}

/// We need this custom deserialization implementation in order to ensure that defaults are applied after deserialization. This is
/// especially needed for moving deprecated modules.
impl<'de> Deserialize<'de> for Config {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct ConfigSerde {
            modules: HashMap<String, Box<dyn ConfigModule>>,
        }

        let config = ConfigSerde::deserialize(deserializer)?;
        let mut config = Config {
            modules: config.modules,
            context: None,
        };
        config.ensure_defaults();
        Ok(config)
    }
}

impl Default for Config {
    fn default() -> Self {
        let mut config = Config {
            modules: HashMap::default(),
            context: std::env::current_dir().ok(),
        };
        config.ensure_defaults();
        config
    }
}

impl Config {
    pub fn from_args(args: CommandLineArgs) -> Self {
        let mut config = Config::from_path(args.config);
        config.apply_overrides(&args.overrides);
        config
    }

    pub fn from_path(config_path: impl AsRef<Path>) -> Self {
        let path_buf = config_path.as_ref().to_path_buf();

        let reader: Box<dyn BufRead>;

        // Check if the path is a URL
        let path = config_path.as_ref().to_string_lossy();
        if is_url(path.as_ref()) {
            #[cfg(feature = "http")]
            {
                reader = Self::url_file_reader(path.parse().unwrap());
            }
            #[cfg(not(feature = "http"))]
            {
                panic!(
                    "HTTP support is not enabled. Please recompile with the `http` feature enabled."
                );
            }
        } else {
            reader = Self::local_file_reader(config_path.as_ref());
        }

        // Parse YAML into Config
        let mut config: Config = serde_yaml::from_reader(reader).unwrap_or_else(|e| {
            panic!(
                "Failed to parse config at {:?}. Original error was: {}",
                path, e
            )
        });
        config.set_context(Some(path_buf));
        config.ensure_defaults();
        config
    }

    /// Ensures that all modules with defaults are present in the config.
    /// Called after deserialization to guarantee that read accessors won't panic
    /// for modules that have sensible defaults.
    pub fn ensure_defaults(&mut self) {
        self.migrate_deprecated_simulation_module();
        self.partitioning_mut();
        self.output_mut();
        self.qsim_mut();
        self.controller_mut();
        self.routing_mut();
        self.replanning_mut();
        self.scoring_mut();
        self.travel_time_calculator_mut();
        self.computational_setup_mut();
        self.network_mut();
        self.population_mut();
        self.vehicles_mut();
        self.transit_mut();
        self.facilities_mut();
        self.ids_mut();
    }

    pub fn set_context(&mut self, context: Option<PathBuf>) {
        self.context = context;
    }

    /// Apply generic key-value overrides to the config, e.g. protofiles.population=path
    fn apply_overrides(&mut self, overrides: &[(String, String)]) {
        info!("Applying overrides: {:?}", overrides);

        for (key, value) in overrides {
            let key_str = key.as_str();

            if let Some(handler) = inventory::iter::<OverrideHandler>().find(|h| h.key == key_str) {
                (handler.apply)(self, value);
            } else {
                warn!("No override handler found for key: {}", key);
            }
        }
    }

    pub fn network(&self) -> &Network {
        self.module::<Network>("network")
            .expect("Network was not set.")
    }

    pub fn network_mut(&mut self) -> &mut Network {
        if !self.modules.contains_key("network") {
            self.modules
                .insert("network".to_string(), Box::new(Network::default()));
        }
        self.module_mut::<Network>("network").unwrap()
    }

    pub fn set_network(&mut self, network: Network) {
        self.modules
            .insert("network".to_string(), Box::new(network));
    }

    pub fn population(&self) -> &Population {
        self.module::<Population>("population")
            .expect("Population was not set.")
    }

    pub fn population_mut(&mut self) -> &mut Population {
        if !self.modules.contains_key("population") {
            self.modules
                .insert("population".to_string(), Box::new(Population::default()));
        }
        self.module_mut::<Population>("population").unwrap()
    }

    pub fn set_population(&mut self, population: Population) {
        self.modules
            .insert("population".to_string(), Box::new(population));
    }

    pub fn vehicles(&self) -> &Vehicles {
        self.module::<Vehicles>("vehicles")
            .expect("Vehicles was not set.")
    }

    pub fn vehicles_mut(&mut self) -> &mut Vehicles {
        if !self.modules.contains_key("vehicles") {
            self.modules
                .insert("vehicles".to_string(), Box::new(Vehicles::default()));
        }
        self.module_mut::<Vehicles>("vehicles").unwrap()
    }

    pub fn set_vehicles(&mut self, vehicles: Vehicles) {
        self.modules
            .insert("vehicles".to_string(), Box::new(vehicles));
    }

    pub fn transit(&self) -> &Transit {
        self.module::<Transit>("transit")
            .expect("Transit was not set.")
    }

    pub fn transit_mut(&mut self) -> &mut Transit {
        if !self.modules.contains_key("transit") {
            self.modules
                .insert("transit".to_string(), Box::new(Transit::default()));
        }
        self.module_mut::<Transit>("transit").unwrap()
    }

    pub fn set_transit(&mut self, transit: Transit) {
        self.modules
            .insert("transit".to_string(), Box::new(transit));
    }

    pub fn facilities(&self) -> &Facilities {
        self.module::<Facilities>("facilities")
            .expect("Facilities was not set.")
    }

    pub fn facilities_mut(&mut self) -> &mut Facilities {
        if !self.modules.contains_key("facilities") {
            self.modules
                .insert("facilities".to_string(), Box::new(Facilities::default()));
        }
        self.module_mut::<Facilities>("facilities").unwrap()
    }

    pub fn set_facilities(&mut self, facilities: Facilities) {
        self.modules
            .insert("facilities".to_string(), Box::new(facilities));
    }

    pub fn ids(&self) -> &Ids {
        self.module::<Ids>("ids").expect("Ids was not set.")
    }

    pub fn ids_mut(&mut self) -> &mut Ids {
        if !self.modules.contains_key("ids") {
            self.modules
                .insert("ids".to_string(), Box::new(Ids::default()));
        }
        self.module_mut::<Ids>("ids").unwrap()
    }

    pub fn set_ids(&mut self, ids: Ids) {
        self.modules.insert("ids".to_string(), Box::new(ids));
    }

    pub fn partitioning(&self) -> &Partitioning {
        self.module::<Partitioning>("partitioning")
            .expect("Partitioning was not set.")
    }

    pub fn partitioning_mut(&mut self) -> &mut Partitioning {
        if !self.modules.contains_key("partitioning") {
            self.modules.insert(
                "partitioning".to_string(),
                Box::new(Partitioning {
                    num_parts: 1,
                    method: PartitionMethod::None,
                    non_main_mode_links: NonMainModeLinks::default(),
                }),
            );
        }
        self.module_mut::<Partitioning>("partitioning").unwrap()
    }

    pub fn set_partitioning(&mut self, partitioning: Partitioning) {
        self.modules
            .insert("partitioning".to_string(), Box::new(partitioning));
    }

    pub fn computational_setup_mut(&mut self) -> &mut ComputationalSetup {
        if !self.modules.contains_key("computational_setup") {
            self.modules.insert(
                "computational_setup".to_string(),
                Box::new(ComputationalSetup::default()),
            );
        }
        self.module_mut::<ComputationalSetup>("computational_setup")
            .unwrap()
    }

    pub fn set_computational_setup(&mut self, setup: ComputationalSetup) {
        self.modules
            .insert("computational_setup".to_string(), Box::new(setup));
    }

    pub fn set_qsim(&mut self, qsim: QSim) {
        self.modules.insert("qsim".to_string(), Box::new(qsim));
    }

    pub fn set_controller(&mut self, controller: Controller) {
        self.modules
            .insert("controller".to_string(), Box::new(controller));
    }

    pub fn output(&self) -> &Output {
        self.module::<Output>("output")
            .expect("Output was not set.")
    }

    pub fn output_mut(&mut self) -> &mut Output {
        if !self.modules.contains_key("output") {
            self.modules
                .insert("output".to_string(), Box::new(Output::default()));
        }
        self.module_mut::<Output>("output").unwrap()
    }

    pub fn set_output(&mut self, output: Output) {
        self.modules.insert("output".to_string(), Box::new(output));
    }

    pub fn routing_mut(&mut self) -> &mut Routing {
        if !self.modules.contains_key("routing") {
            self.modules
                .insert("routing".to_string(), Box::new(Routing::default()));
        }
        self.module_mut::<Routing>("routing").unwrap()
    }

    pub fn set_routing(&mut self, routing: Routing) {
        self.modules
            .insert("routing".to_string(), Box::new(routing));
    }

    pub fn replanning(&self) -> &Replanning {
        self.module::<Replanning>("replanning")
            .expect("Replanning was not set.")
    }

    pub fn replanning_mut(&mut self) -> &mut Replanning {
        if !self.modules.contains_key("replanning") {
            self.modules
                .insert("replanning".to_string(), Box::new(Replanning::default()));
        }
        self.module_mut::<Replanning>("replanning").unwrap()
    }

    pub fn set_replanning(&mut self, replanning: Replanning) {
        self.modules
            .insert("replanning".to_string(), Box::new(replanning));
    }

    pub fn scoring(&self) -> &Scoring {
        self.module::<Scoring>("scoring")
            .expect("Scoring was not set.")
    }

    pub fn scoring_mut(&mut self) -> &mut Scoring {
        if !self.modules.contains_key("scoring") {
            self.modules
                .insert("scoring".to_string(), Box::new(Scoring::default()));
        }
        self.module_mut::<Scoring>("scoring").unwrap()
    }

    pub fn set_scoring(&mut self, scoring: Scoring) {
        self.modules
            .insert("scoring".to_string(), Box::new(scoring));
    }

    pub fn travel_time_calculator(&self) -> &TravelTimeCalculator {
        self.module::<TravelTimeCalculator>("travel_time_calculator")
            .expect("TravelTimeCalculator was not set.")
    }

    pub fn travel_time_calculator_mut(&mut self) -> &mut TravelTimeCalculator {
        if !self.modules.contains_key("travel_time_calculator") {
            self.modules.insert(
                "travel_time_calculator".to_string(),
                Box::new(TravelTimeCalculator::default()),
            );
        }
        self.module_mut::<TravelTimeCalculator>("travel_time_calculator")
            .unwrap()
    }

    pub fn set_travel_time_calculator(&mut self, calculator: TravelTimeCalculator) {
        self.modules
            .insert("travel_time_calculator".to_string(), Box::new(calculator));
    }

    pub fn qsim(&self) -> &QSim {
        self.module::<QSim>("qsim").expect("QSim was not set.")
    }

    pub fn qsim_mut(&mut self) -> &mut QSim {
        if !self.modules.contains_key("qsim") {
            self.modules
                .insert("qsim".to_string(), Box::new(QSim::default()));
        }
        self.module_mut::<QSim>("qsim").unwrap()
    }

    pub fn controller(&self) -> &Controller {
        self.module::<Controller>("controller")
            .expect("ControllerConfig was not set.")
    }

    pub fn controller_mut(&mut self) -> &mut Controller {
        if !self.modules.contains_key("controller") {
            self.modules
                .insert("controller".to_string(), Box::new(Controller::default()));
        }
        self.module_mut::<Controller>("controller").unwrap()
    }

    pub fn routing(&self) -> &Routing {
        self.module::<Routing>("routing")
            .expect("Routing was not set.")
    }

    pub fn computational_setup(&self) -> &ComputationalSetup {
        self.module::<ComputationalSetup>("computational_setup")
            .expect("ComputationalSetup was not set.")
    }

    fn module<T: 'static>(&self, key: &str) -> Option<&T> {
        self.modules
            .get(key)
            .map(|boxed| boxed.as_ref().as_any().downcast_ref::<T>().unwrap())
    }

    fn module_mut<T: 'static>(&mut self, key: &str) -> Option<&mut T> {
        self.modules
            .get_mut(key)
            .map(|boxed| boxed.as_mut().as_any_mut().downcast_mut::<T>().unwrap())
    }

    pub fn context(&self) -> &Option<PathBuf> {
        &self.context
    }

    fn migrate_deprecated_simulation_module(&mut self) {
        let simulation = self
            .modules
            .get("simulation")
            .and_then(|module| module.as_ref().as_any().downcast_ref::<Simulation>())
            .cloned();

        if let Some(simulation) = simulation {
            warn!(
                "The config module `simulation` is deprecated. Use `qsim` and `controller` instead."
            );

            if !self.modules.contains_key("qsim") {
                self.set_qsim(QSim::from(&simulation));
            }
            if !self.modules.contains_key("controller") {
                self.set_controller(Controller::from(&simulation));
            }

            self.modules.remove("simulation");
        }
    }

    fn local_file_reader(config_path: impl AsRef<Path>) -> Box<dyn BufRead> {
        // Open the config file from the local file system
        let file = File::open(&config_path).unwrap_or_else(|e| {
            panic!(
                "Failed to open config file at {:?}. Original error was {}",
                config_path.as_ref(),
                e
            );
        });
        // Wrap the file in a BufReader for YAML parsing
        Box::new(BufReader::new(file))
    }

    #[cfg(feature = "http")]
    fn url_file_reader(url: Url) -> Box<dyn BufRead> {
        // Make a blocking request to get the config file and read the response body
        let resp = reqwest::blocking::get(url).expect("Failed to fetch config URL");
        let bytes = resp.bytes().expect("Failed to read response body").to_vec();
        // Wrap the response bytes in a BufReader for YAML parsing
        Box::new(BufReader::new(std::io::Cursor::new(bytes)))
    }
}

pub fn write_config(config: &Config, output_path: PathBuf) {
    let output_config = output_path.join("output_config.yml");
    let file = File::create(&output_config).expect("Failed to create output config file");
    let writer = BufWriter::new(file);
    serde_yaml::to_writer(writer, config).expect("Failed to write output config file");
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Network {
    pub path: Option<PathBuf>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Population {
    pub path: Option<PathBuf>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Vehicles {
    pub path: Option<PathBuf>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Transit {
    pub schedule_path: Option<PathBuf>,
    /// Transit vehicles and their types. Only needed if `simulation` is
    /// [`TransitSimulation::ScheduleBased`].
    #[serde(default)]
    pub vehicles_path: Option<PathBuf>,
    #[serde(default)]
    pub simulation: TransitSimulation,
    /// Leg modes whose legs ride transit vehicles if `simulation` is
    /// [`TransitSimulation::ScheduleBased`], like MATSim's `transit.transitModes`.
    #[serde(default = "default_transit_passenger_modes")]
    pub passenger_modes: Vec<String>,
}

impl Default for Transit {
    fn default() -> Self {
        Self {
            schedule_path: None,
            vehicles_path: None,
            simulation: TransitSimulation::default(),
            passenger_modes: default_transit_passenger_modes(),
        }
    }
}

fn default_transit_passenger_modes() -> Vec<String> {
    vec!["pt".to_string()]
}

/// How the mobsim moves passengers of transit legs.
#[derive(PartialEq, Eq, Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub enum TransitSimulation {
    /// Transit legs are teleported with the travel time of their route.
    #[default]
    Teleported,
    /// Transit vehicles move from stop to stop according to the schedule, without using the
    /// network, and passengers board and alight them, like SBB's deterministic transit simulation
    /// in MATSim.
    ScheduleBased,
}

fn parse_transit_simulation(value: &str) -> TransitSimulation {
    match value.to_lowercase().replace(['-', '_'], "").as_str() {
        "teleported" => TransitSimulation::Teleported,
        "schedulebased" => TransitSimulation::ScheduleBased,
        _ => panic!("Invalid transit simulation: {}", value),
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Facilities {
    pub path: Option<PathBuf>,
    #[serde(default)]
    pub modal_link_selection: ModalLinkSelection,
}

/// How the modal link of a facility, i.e. its access and egress link for a mode, is chosen. This
/// applies to activity facilities and to the link wrappers of activities without a facility.
#[derive(PartialEq, Eq, Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub enum ModalLinkSelection {
    #[default]
    BaseLinkFirst,
    NearestLink,
}

fn parse_modal_link_selection(value: &str) -> ModalLinkSelection {
    match value.to_lowercase().replace(['-', '_'], "").as_str() {
        "baselinkfirst" => ModalLinkSelection::BaseLinkFirst,
        "nearestlink" => ModalLinkSelection::NearestLink,
        _ => panic!("Invalid modal_link_selection: {}", value),
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Ids {
    pub path: Option<PathBuf>,
}

register_override!("network.path", |config, value| {
    config.network_mut().path = Some(PathBuf::from(value));
});

register_override!("population.path", |config, value| {
    config.population_mut().path = Some(PathBuf::from(value));
});

register_override!("vehicles.path", |config, value| {
    config.vehicles_mut().path = Some(PathBuf::from(value));
});

register_override!("transit.schedule_path", |config, value| {
    config.transit_mut().schedule_path = Some(PathBuf::from(value));
});

register_override!("transit.vehicles_path", |config, value| {
    config.transit_mut().vehicles_path = Some(PathBuf::from(value));
});

register_override!("transit.simulation", |config, value| {
    config.transit_mut().simulation = parse_transit_simulation(value);
});

register_override!("facilities.path", |config, value| {
    config.facilities_mut().path = Some(PathBuf::from(value));
});

register_override!("facilities.modal_link_selection", |config, value| {
    config.facilities_mut().modal_link_selection = parse_modal_link_selection(value);
});

register_override!("ids.path", |config, value| {
    config.set_ids(Ids {
        path: Some(PathBuf::from(value)),
    });
});

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Partitioning {
    pub num_parts: u32,
    pub method: PartitionMethod,
    #[serde(default)]
    pub non_main_mode_links: NonMainModeLinks,
}

/// How a partitioning method treats links that allow none of the `qsim.main_modes`, e.g. links of
/// a transit network whose vehicles are teleported. No vehicle moves on such links, but agents may
/// still perform activities on them, e.g. `pt interaction`.
#[derive(PartialEq, Debug, ValueEnum, Clone, Copy, Serialize, Deserialize, Default)]
pub enum NonMainModeLinks {
    /// Only the nodes incident to a link that allows a main mode are partitioned, using only these
    /// links. Afterwards, every other node is assigned to the partition of the nearest partitioned
    /// node. If no link allows a main mode, all nodes and links are partitioned.
    #[default]
    AssignAfterPartitioningToNearest,
    /// All nodes and links are partitioned, regardless of their modes.
    Include,
}

register_override!("partitioning.num_parts", |config, value| {
    if let Ok(v) = value.parse() {
        config.partitioning_mut().num_parts = v;
        // replace some configuration if we get a partition from the outside. This is interesting for testing
        let out_dir = format!("{}-{v}", config.output().output_dir.to_str().unwrap());
        config.output_mut().output_dir = out_dir.into();
    }
});

// Only `none` can be set from the command line. It makes the simulation use the partitions stored
// in the network file. Metis options have to be configured in the config file.
register_override!("partitioning.method", |config, value| {
    config.partitioning_mut().method = match value.to_lowercase().as_str() {
        "none" => PartitionMethod::None,
        _ => panic!(
            "Invalid partitioning method: {}. Only `none` can be overridden, configure Metis in the config file.",
            value
        ),
    };
});

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Output {
    pub output_dir: PathBuf,
    #[serde(default)]
    pub overwrite_files: OverwriteFiles,
    #[serde(default)]
    pub profiling: Profiling,
    #[serde(default)]
    pub logging: Logging,
    #[serde(default)]
    pub write_events: WriteEvents,
}

impl Default for Output {
    fn default() -> Self {
        Self {
            output_dir: "./output".parse().unwrap(),
            overwrite_files: OverwriteFiles::FailIfDirectoryExists,
            profiling: Profiling::None,
            logging: Logging::Info,
            write_events: WriteEvents::File,
        }
    }
}

register_override!("output.output_dir", |config, value| {
    config.output_mut().output_dir = PathBuf::from(value);
});

register_override!("output.overwrite_files", |config, value| {
    config.output_mut().overwrite_files = parse_overwrite_file(value);
});

register_override!("output.write_events", |config, value| {
    config.output_mut().write_events = match value.to_lowercase().as_str() {
        "none" => WriteEvents::None,
        "file" => WriteEvents::File,
        _ => panic!("Invalid write_events: {}", value),
    };
});

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Routing {
    pub mode: RoutingMode,
    #[serde(default)]
    pub network_modes: Vec<String>,
    #[serde(default = "default_access_egress_mode")]
    pub access_egress_mode: String,
    #[serde(
        default = "default_teleported_mode_params",
        deserialize_with = "deserialize_teleported_mode_params"
    )]
    pub teleported_mode_params: Vec<TeleportedParams>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct TeleportedParams {
    pub mode: String,
    pub beeline_distance_factor: f64,
    pub teleported_mode_speed: f64,
}

fn default_access_egress_mode() -> String {
    "walk".to_string()
}

fn default_walk_teleported_params() -> TeleportedParams {
    TeleportedParams {
        mode: "walk".to_string(),
        beeline_distance_factor: 1.3,
        teleported_mode_speed: 3.0 / 3.6,
    }
}

fn default_teleported_mode_params() -> Vec<TeleportedParams> {
    vec![default_walk_teleported_params()]
}

fn deserialize_teleported_mode_params<'de, D>(
    deserializer: D,
) -> Result<Vec<TeleportedParams>, D::Error>
where
    D: Deserializer<'de>,
{
    let mut params = Vec::<TeleportedParams>::deserialize(deserializer)?;
    let last_walk_index = params.iter().rposition(|param| param.mode == "walk");

    if let Some(last_walk_index) = last_walk_index {
        params = params
            .into_iter()
            .enumerate()
            .filter_map(|(index, param)| {
                (param.mode != "walk" || index == last_walk_index).then_some(param)
            })
            .collect();
    } else {
        params.push(default_walk_teleported_params());
    }

    Ok(params)
}

register_override!("routing.mode", |config, value| {
    config.routing_mut().mode = match value.to_lowercase().as_str() {
        "ad-hoc" | "adhoc" => RoutingMode::AdHoc,
        "use-plans" | "useplans" => RoutingMode::UsePlans,
        _ => panic!("Invalid routing mode: {}", value),
    };
});

impl Default for Routing {
    fn default() -> Self {
        Routing {
            mode: RoutingMode::UsePlans,
            network_modes: Vec::new(),
            access_egress_mode: default_access_egress_mode(),
            teleported_mode_params: default_teleported_mode_params(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Replanning {
    pub fraction_of_iterations_to_disable_innovation: f64,
    pub max_agent_plan_memory: u32,
    pub plan_selector_for_removal: String,
    pub strategy_settings: Vec<StrategySetting>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Builder)]
pub struct StrategySetting {
    pub name: String,
    pub weight: f64,
    pub subpopulation: String,
}

impl StrategySetting {
    pub fn new(name: String, weight: f64, subpopulation: String) -> Self {
        Self {
            name,
            weight,
            subpopulation,
        }
    }
}

register_override!(
    "replanning.fraction_of_iterations_to_disable_innovation",
    |config, value| {
        config
            .replanning_mut()
            .fraction_of_iterations_to_disable_innovation = value.parse().unwrap();
    }
);

register_override!("replanning.max_agent_plan_memory", |config, value| {
    config.replanning_mut().max_agent_plan_memory = value.parse().unwrap();
});

register_override!("replanning.plan_selector_for_removal", |config, value| {
    config.replanning_mut().plan_selector_for_removal = value.to_string();
});

impl Default for Replanning {
    fn default() -> Self {
        Self {
            fraction_of_iterations_to_disable_innovation: 1.0,
            max_agent_plan_memory: 5,
            plan_selector_for_removal: WORST_SCORE_STRATEGY_NAME.to_string(),
            strategy_settings: vec![StrategySetting {
                name: KEEP_LAST_SELECTED_STRATEGY_NAME.to_string(),
                weight: 1.0,
                subpopulation: "person".to_string(),
            }],
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Scoring {
    pub mode: ScoringMode,
    pub write_experienced_plans: bool,
    pub activity_params: Vec<ActivityParameter>,
    pub mode_params: Vec<ModeParameter>,
    pub agent_params: Vec<AgentParameter>,
    pub fraction_of_iterations_to_start_score_msa: Option<f64>,
}

/// `Disabled` skips backpacking and scoring entirely. Selected plans then receive no score.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Default)]
pub enum ScoringMode {
    #[default]
    Enabled,
    Disabled,
}

impl Default for Scoring {
    fn default() -> Self {
        Self {
            mode: ScoringMode::Enabled,
            write_experienced_plans: true,
            activity_params: vec![
                ActivityParameter::default_for_activity_type("home"),
                ActivityParameter::default_for_activity_type("work"),
                ActivityParameter::default_for_activity_type("leisure"),
                ActivityParameter::default_for_activity_type("shop"),
                ActivityParameter::default_for_activity_type("errands"),
            ],
            mode_params: vec![
                ModeParameter::default_for_mode("car"),
                ModeParameter::default_for_mode("walk"),
                ModeParameter::default_for_mode("ride"),
                ModeParameter::default_for_mode("freight"),
                ModeParameter::default_for_mode("bike"),
            ],
            agent_params: vec![AgentParameter::default()],
            fraction_of_iterations_to_start_score_msa: None,
        }
    }
}

register_override!("scoring.mode", |config, value| {
    config.scoring_mut().mode = match value.to_lowercase().as_str() {
        "enabled" => ScoringMode::Enabled,
        "disabled" => ScoringMode::Disabled,
        _ => panic!("Invalid scoring mode: {}", value),
    };
});

register_override!("scoring.write_experienced_plans", |config, value| {
    config.scoring_mut().write_experienced_plans = value.parse().unwrap();
});

register_override!(
    "scoring.fraction_of_iterations_to_start_score_msa",
    |config, value| {
        config
            .scoring_mut()
            .fraction_of_iterations_to_start_score_msa = Some(value.parse().unwrap());
    }
);

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ActivityParameter {
    pub activity_type: String,
    #[serde(default = "f64_value_1_0")]
    pub typical_duration_s: f64,
}

impl ActivityParameter {
    pub fn default_for_activity_type(activity_type: &str) -> Self {
        Self {
            activity_type: activity_type.to_string(),
            typical_duration_s: 1.0,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ModeParameter {
    pub mode: String,
    pub marginal_utility_of_traveling: f64, // utils/hour
    pub marginal_utility_of_distance: f64,  // utils/meters
    pub monetary_distance_cost_rate: f64,   // money/meter
    pub daily_money_constant: f64,          // money/day
    pub daily_utility_constant: f64,        // utils/day
    pub constant: f64,
}

impl ModeParameter {
    pub fn default_for_mode(mode: &str) -> Self {
        let mut default_mode_params = Self::default();
        default_mode_params.mode = mode.to_string();
        default_mode_params
    }
}

impl Default for ModeParameter {
    fn default() -> Self {
        Self {
            mode: "walk".to_string(),
            marginal_utility_of_traveling: -6.0,
            marginal_utility_of_distance: 0.0,
            monetary_distance_cost_rate: 0.0,
            daily_money_constant: 0.0,
            daily_utility_constant: 0.0,
            constant: 0.0,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct AgentParameter {
    pub subpopulation: String,
    pub late_arrival: f64,              // utils/hour
    pub early_departure: f64,           // utils/hour
    pub performing: f64,                // utils/hour
    pub waiting: f64,                   // utils/hour
    pub marginal_utility_of_money: f64, // utils/money
    pub aborted_plan_score: f64,        // utils/hour
}

impl Default for AgentParameter {
    fn default() -> Self {
        Self {
            subpopulation: "person".to_string(),
            late_arrival: -18.0,
            early_departure: -0.0,
            performing: 6.0,
            waiting: -0.0,
            marginal_utility_of_money: 1.0,
            aborted_plan_score: -18.0,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct QSim {
    pub start_time: u32,
    pub end_time: u32,
    pub ticks_per_second: u32,
    pub sample_size: f64,
    pub stuck_threshold: u32,
    pub main_modes: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct TravelTimeCalculator {
    /// Width of a travel-time interval in seconds. Like MATSim's `travelTimeBinSize`, fractions
    /// such as `0.5` are allowed. The width must be a whole multiple of the tick
    /// length (`1 / qsim.ticks_per_second`), so that bin boundaries coincide with tick boundaries.
    pub bin_size: f64,
}

impl TravelTimeCalculator {
    /// Validates `bin_size` against the tick length and returns it as an exact duration.
    pub fn bin_duration(&self, ticks_per_second: u32) -> Result<Duration, String> {
        if !self.bin_size.is_finite() || self.bin_size <= 0.0 {
            return Err(format!(
                "travel_time_calculator.bin_size must be finite and greater than 0, got {}",
                self.bin_size
            ));
        }
        if ticks_per_second == 0 {
            return Err("qsim.ticks_per_second must be greater than 0".to_string());
        }

        let ticks = self.bin_size * f64::from(ticks_per_second);
        let whole_ticks = ticks.round();
        // Decimal inputs such as 0.1 are not exactly representable in binary. Parsing and the
        // multiplication with an integer each round by at most half an ULP, so a few ULPs of
        // relative tolerance accept those values without accepting a genuine fraction of a tick.
        if whole_ticks < 1.0 || (ticks - whole_ticks).abs() > 4.0 * f64::EPSILON * whole_ticks {
            return Err(format!(
                "travel_time_calculator.bin_size ({} s) must be a whole multiple of the tick length (1/{} s)",
                self.bin_size, ticks_per_second
            ));
        }

        // Compute the width from the integer tick count, so it carries no binary rounding error.
        // Every tick count that yields a representable width converts exactly to u128; larger
        // values saturate and are rejected by the range check.
        let tick_nanos = (whole_ticks as u128).saturating_mul(1_000_000_000);
        let ticks_per_second = u128::from(ticks_per_second);
        let nanos = u64::try_from(tick_nanos / ticks_per_second).map_err(|_| {
            format!(
                "travel_time_calculator.bin_size ({} s) exceeds the supported time range",
                self.bin_size
            )
        })?;
        if tick_nanos % ticks_per_second != 0 {
            return Err(format!(
                "travel_time_calculator.bin_size ({} s) cannot be represented in whole nanoseconds with qsim.ticks_per_second = {}",
                self.bin_size, ticks_per_second
            ));
        }
        Ok(Duration::from_nanos(nanos))
    }
}

impl Default for TravelTimeCalculator {
    fn default() -> Self {
        Self { bin_size: 900.0 }
    }
}

register_override!("travel_time_calculator.bin_size", |config, value| {
    config.travel_time_calculator_mut().bin_size = value.parse().unwrap();
});

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Controller {
    pub first_iteration: u32,
    pub last_iteration: u32,
    pub write_events_interval: u32,
    pub write_plans_interval: u32,
    pub compression_type: CompressionType,
}

impl Controller {
    pub fn should_write_plans(&self, iteration: u32, is_last_iteration: bool) -> bool {
        is_last_iteration || iteration % self.write_plans_interval == 0
    }
}

#[deprecated(note = "Use `QSim` and `Controller` instead. This will be removed in the future.")]
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Simulation {
    pub first_iteration: u32,
    pub last_iteration: u32,
    pub write_events_interval: u32,
    pub write_plans_interval: u32,
    pub start_time: u32,
    pub end_time: u32,
    pub ticks_per_second: u32,
    pub sample_size: f64,
    pub stuck_threshold: u32,
    pub main_modes: Vec<String>,
}

impl From<&Simulation> for QSim {
    fn from(value: &Simulation) -> Self {
        Self {
            start_time: value.start_time,
            end_time: value.end_time,
            ticks_per_second: value.ticks_per_second,
            sample_size: value.sample_size,
            stuck_threshold: value.stuck_threshold,
            main_modes: value.main_modes.clone(),
        }
    }
}

impl From<&Simulation> for Controller {
    fn from(value: &Simulation) -> Self {
        Self {
            first_iteration: value.first_iteration,
            last_iteration: value.last_iteration,
            write_events_interval: value.write_events_interval,
            write_plans_interval: value.write_plans_interval,
            compression_type: CompressionType::Proto,
        }
    }
}

register_override!("qsim.start_time", |config, value| {
    config.qsim_mut().start_time = value.parse().unwrap();
});

register_override!("qsim.end_time", |config, value| {
    config.qsim_mut().end_time = value.parse().unwrap();
});

register_override!("qsim.ticks_per_second", |config, value| {
    config.qsim_mut().ticks_per_second = value.parse().unwrap();
});

register_override!("qsim.sample_size", |config, value| {
    config.qsim_mut().sample_size = value.parse().unwrap();
});

register_override!("qsim.stuck_threshold", |config, value| {
    config.qsim_mut().stuck_threshold = value.parse().unwrap();
});

register_override!("qsim.main_modes", |config, value| {
    config.qsim_mut().main_modes = value
        .split(',')
        .map(str::trim)
        .filter(|mode| !mode.is_empty())
        .map(ToString::to_string)
        .collect();
});

register_override!("controller.first_iteration", |config, value| {
    config.controller_mut().first_iteration = value.parse().unwrap();
});

register_override!("controller.last_iteration", |config, value| {
    config.controller_mut().last_iteration = value.parse().unwrap();
});

register_override!("controller.write_events_interval", |config, value| {
    config.controller_mut().write_events_interval = value.parse().unwrap();
});

register_override!("controller.write_plans_interval", |config, value| {
    config.controller_mut().write_plans_interval = value.parse().unwrap();
});

register_override!("controller.compression_type", |config, value| {
    config.controller_mut().compression_type = parse_compression_type(value);
});

#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
#[serde(default)]
pub struct ComputationalSetup {
    pub global_sync: bool,
    /// The number of threads to be used for the tokio runtime by the adapter.
    pub adapter_worker_threads: u32,
    /// The number of threads to be used by the replanning pool. 0 uses Rayon's default.
    pub replanning_threads: u32,
    /// The number of threads to be used by the scoring pool. 0 uses Rayon's default.
    pub scoring_threads: u32,
    pub retry_time_seconds: u64,
    pub random_seed: u64,
}

register_override!(
    "computational_setup.adapter_worker_threads",
    |config, value| {
        config.computational_setup_mut().adapter_worker_threads = value.parse().unwrap();
    }
);

register_override!("computational_setup.replanning_threads", |config, value| {
    config.computational_setup_mut().replanning_threads = value.parse().unwrap();
});

register_override!("computational_setup.scoring_threads", |config, value| {
    config.computational_setup_mut().scoring_threads = value.parse().unwrap();
});

register_override!("computational_setup.global_sync", |config, value| {
    config.computational_setup_mut().global_sync = value.parse().unwrap();
});

register_override!("computational_setup.random_seed", |config, value| {
    config.computational_setup_mut().random_seed = value.parse().unwrap();
});

impl Default for ComputationalSetup {
    fn default() -> Self {
        Self {
            global_sync: false,
            adapter_worker_threads: 3,
            replanning_threads: 0,
            scoring_threads: 0,
            retry_time_seconds: 600,
            random_seed: DEFAULT_RANDOM_SEED,
        }
    }
}

#[typetag::serde(tag = "type")]
pub trait ConfigModule: Debug + Send + Sync + DynClone {
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

#[typetag::serde]
impl ConfigModule for Network {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

#[typetag::serde]
impl ConfigModule for Population {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

#[typetag::serde]
impl ConfigModule for Vehicles {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

#[typetag::serde]
impl ConfigModule for Transit {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

#[typetag::serde]
impl ConfigModule for Facilities {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

#[typetag::serde]
impl ConfigModule for Ids {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

#[typetag::serde]
impl ConfigModule for Partitioning {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

#[typetag::serde]
impl ConfigModule for Output {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

#[typetag::serde]
impl ConfigModule for Routing {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

#[typetag::serde]
impl ConfigModule for Replanning {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

#[typetag::serde]
impl ConfigModule for Scoring {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

#[typetag::serde]
impl ConfigModule for TravelTimeCalculator {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

#[typetag::serde]
impl ConfigModule for QSim {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

#[typetag::serde]
impl ConfigModule for Controller {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

#[typetag::serde]
impl ConfigModule for Simulation {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

#[typetag::serde]
impl ConfigModule for ComputationalSetup {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

// This is needed to allow cloning of the trait object and thus cloning of the Config.
dyn_clone::clone_trait_object!(ConfigModule);

impl Default for QSim {
    fn default() -> Self {
        Self {
            start_time: 0,
            end_time: 86400,
            ticks_per_second: 1,
            sample_size: 1.0,
            stuck_threshold: 10,
            main_modes: vec![],
        }
    }
}

impl Default for Controller {
    fn default() -> Self {
        Self {
            first_iteration: 0,
            last_iteration: 1000,
            write_events_interval: 50,
            write_plans_interval: 50,
            compression_type: CompressionType::Proto,
        }
    }
}

impl Default for Simulation {
    fn default() -> Self {
        let qsim = QSim::default();
        let controller = Controller::default();
        Self {
            first_iteration: controller.first_iteration,
            last_iteration: controller.last_iteration,
            write_events_interval: controller.write_events_interval,
            write_plans_interval: controller.write_plans_interval,
            start_time: qsim.start_time,
            end_time: qsim.end_time,
            ticks_per_second: qsim.ticks_per_second,
            sample_size: qsim.sample_size,
            stuck_threshold: qsim.stuck_threshold,
            main_modes: qsim.main_modes,
        }
    }
}

#[derive(PartialEq, Debug, ValueEnum, Clone, Copy, Serialize, Deserialize)]
pub enum RoutingMode {
    AdHoc,
    UsePlans,
}

#[derive(PartialEq, Debug, ValueEnum, Clone, Copy, Serialize, Deserialize, Default)]
pub enum OverwriteFiles {
    DeleteDirectoryIfExists,
    #[default]
    FailIfDirectoryExists,
    OverwriteExistingFiles,
}

fn parse_overwrite_file(value: &str) -> OverwriteFiles {
    match value.to_lowercase().replace(['-', '_'], "").as_str() {
        "deletedirectoryifexists" => OverwriteFiles::DeleteDirectoryIfExists,
        "failifdirectoryexists" => OverwriteFiles::FailIfDirectoryExists,
        "overwriteexistingfiles" => OverwriteFiles::OverwriteExistingFiles,
        _ => panic!("Invalid overwrite_files mode: {}", value),
    }
}

#[derive(PartialEq, Debug, Clone, Serialize, Deserialize)]
pub enum PartitionMethod {
    Metis(MetisOptions),
    None,
}

#[derive(PartialEq, Debug, Clone, Serialize, Deserialize, Default)]
pub enum Profiling {
    #[default]
    None,
    CSV(ProfilingLevel),
    Parquet(ParquetProfilingLevel),
}

/// Have this extra layer of log level enum, as tracing subscriber has no
/// off/none option by default. At least it can't be parsed
#[derive(PartialEq, Debug, Clone, Serialize, Deserialize, Default)]
pub enum Logging {
    #[default]
    None,
    Info,
}

#[derive(PartialEq, Debug, Clone, Serialize, Deserialize, Default)]
pub enum WriteEvents {
    None,
    // for backward compatability, we still allow "Proto" and "XmlGz"
    #[default]
    #[serde(alias = "Proto", alias = "XmlGz")]
    File,
}

#[derive(PartialEq, Debug, ValueEnum, Clone, Copy, Serialize, Deserialize, Default)]
pub enum CompressionType {
    None,
    #[serde(alias = "XmlGz", alias = "Gz")]
    Gz,
    #[default]
    Proto,
    #[serde(alias = "XmlZst", alias = "Zst")]
    Zst,
}

impl CompressionType {
    pub fn extension(self) -> &'static str {
        match self {
            Self::None => "xml",
            Self::Gz => "xml.gz",
            Self::Proto => "binpb",
            Self::Zst => "xml.zst",
        }
    }

    pub fn with_extension(self, stem: &str) -> String {
        format!("{stem}.{}", self.extension())
    }

    pub fn is_protobuf(self) -> bool {
        self == Self::Proto
    }
}

fn parse_compression_type(value: &str) -> CompressionType {
    match value.to_lowercase().replace(['-', '_'], "").as_str() {
        "none" | "xml" => CompressionType::None,
        "gz" | "gzip" | "xmlgz" => CompressionType::Gz,
        "protobuf" | "proto" | "binpb" => CompressionType::Proto,
        "zst" | "zstd" | "xmlzst" => CompressionType::Zst,
        _ => panic!("Invalid compression_type: {}", value),
    }
}

#[derive(PartialEq, Debug, Clone, Serialize, Deserialize, Default)]
pub struct ParquetProfilingLevel {
    #[serde(default = "default_profiling_level")]
    pub level: String,
    #[serde(default = "default_parquet_batch_size")]
    pub batch_size: usize,
}

fn default_parquet_batch_size() -> usize {
    50_000
}

#[derive(PartialEq, Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProfilingLevel {
    #[serde(default = "default_profiling_level")]
    pub level: String,
}

impl ProfilingLevel {
    pub fn create_tracing_level(&self) -> Level {
        match self.level.as_str() {
            "INFO" => Level::INFO,
            "TRACE" => Level::TRACE,
            _ => panic!("{} not yet implemented as profiling level!", self.level),
        }
    }
}

impl ParquetProfilingLevel {
    pub fn create_tracing_level(&self) -> Level {
        match self.level.as_str() {
            "INFO" => Level::INFO,
            "TRACE" => Level::TRACE,
            _ => panic!("{} not yet implemented as profiling level!", self.level),
        }
    }
}

#[derive(PartialEq, Debug, Clone, Serialize, Deserialize)]
pub struct MetisOptions {
    #[serde(default = "default_vertex_weight")]
    pub vertex_weight: Vec<VertexWeight>,
    #[serde(default = "edge_weight_constant")]
    pub edge_weight: EdgeWeight,
    /// The largest weight of a partition relative to the mean weight of all partitions, as METIS's load imbalance:
    /// 1.03 allows partitions 3 % heavier than the mean. Must be at least 1.
    #[serde(default = "f32_value_1_03")]
    pub imbalance_factor: f32,
    #[serde(default = "u32_value_100")]
    pub iteration_number: u32,
    #[serde(default = "bool_value_false")]
    pub contiguous: bool,
    /// Makes METIS try to minimize the maximum number of neighbors of a partition.
    #[serde(default = "bool_value_true")]
    pub min_conn: bool,
}

#[derive(PartialEq, Debug, ValueEnum, Clone, Copy, Serialize, Deserialize)]
pub enum VertexWeight {
    InLinkCapacity,
    InLinkCount,
    Constant,
    PreComputed,
}

#[derive(PartialEq, Debug, ValueEnum, Clone, Copy, Serialize, Deserialize)]
pub enum EdgeWeight {
    Capacity,
    Constant,
}

impl Default for MetisOptions {
    fn default() -> Self {
        MetisOptions {
            vertex_weight: vec![],
            edge_weight: EdgeWeight::Constant,
            imbalance_factor: 1.03,
            iteration_number: 10,
            contiguous: true,
            min_conn: true,
        }
    }
}

impl MetisOptions {
    pub fn set_imbalance_factor(mut self, imbalance_factor: f32) -> Self {
        self.imbalance_factor = imbalance_factor;
        self
    }

    pub fn add_vertex_weight(mut self, vertex_weight: VertexWeight) -> Self {
        self.vertex_weight.push(vertex_weight);
        self
    }

    pub fn set_edge_weight(mut self, edge_weight: EdgeWeight) -> Self {
        self.edge_weight = edge_weight;
        self
    }

    pub fn set_iteration_number(mut self, iteration_number: u32) -> Self {
        self.iteration_number = iteration_number;
        self
    }

    /// METIS's `ufactor`: the allowed excess over the mean weight in thousandths, at least 1.
    pub fn ufactor(&self) -> usize {
        assert!(
            self.imbalance_factor >= 1.,
            "imbalance_factor must be at least 1, e.g. 1.03 to allow partitions 3 % heavier than the mean, but is {}",
            self.imbalance_factor
        );
        (((self.imbalance_factor - 1.) * 1000.).round() as usize).max(1)
    }

    pub fn set_contiguous(mut self, contiguous: bool) -> Self {
        self.contiguous = contiguous;
        self
    }

    pub fn set_min_conn(mut self, min_conn: bool) -> Self {
        self.min_conn = min_conn;
        self
    }
}

fn f32_value_1_03() -> f32 {
    1.03
}

fn f64_value_1_0() -> f64 {
    1.0
}

fn edge_weight_constant() -> EdgeWeight {
    EdgeWeight::Constant
}

fn u32_value_100() -> u32 {
    100
}

fn bool_value_false() -> bool {
    false
}

fn bool_value_true() -> bool {
    true
}

fn default_vertex_weight() -> Vec<VertexWeight> {
    vec![InLinkCapacity]
}

fn default_profiling_level() -> String {
    String::from("INFO")
}

#[cfg(test)]
mod tests {
    use crate::simulation::config;
    use crate::simulation::config::Output;
    use crate::simulation::config::OverwriteFiles;
    use crate::simulation::config::PathBuf;
    use crate::simulation::config::Profiling;
    use crate::simulation::config::WriteEvents;
    use crate::simulation::config::{
        ActivityParameter, AgentParameter, CommandLineArgs, CompressionType, ComputationalSetup,
        Config, Controller, EdgeWeight, MetisOptions, ModeParameter, NonMainModeLinks,
        PartitionMethod, Partitioning, QSim, Replanning, Routing, Scoring, ScoringMode,
        StrategySetting, TeleportedParams, TravelTimeCalculator, VertexWeight, parse_key_val,
    };
    use crate::simulation::config::{
        Ids, Network, Population, Transit, TransitSimulation, Vehicles,
    };
    use crate::simulation::config::{Logging, ModalLinkSelection, RoutingMode};
    use crate::simulation::replanning::{
        KEEP_LAST_SELECTED_STRATEGY_NAME, WORST_SCORE_STRATEGY_NAME,
    };
    use std::io::Write;
    use std::time::Duration;
    use tempfile::NamedTempFile;

    #[test]
    fn read_from_yaml() {
        let mut config = Config::default();
        let partitioning = Partitioning {
            num_parts: 1,
            method: PartitionMethod::Metis(MetisOptions {
                vertex_weight: vec![VertexWeight::InLinkCount, VertexWeight::InLinkCapacity],
                edge_weight: EdgeWeight::Constant,
                imbalance_factor: 1.02,
                iteration_number: 100,
                contiguous: true,
                min_conn: true,
            }),
            non_main_mode_links: NonMainModeLinks::Include,
        };
        let computational_setup = ComputationalSetup {
            global_sync: true,
            adapter_worker_threads: 42,
            replanning_threads: 7,
            scoring_threads: 0,
            retry_time_seconds: 41,
            random_seed: config::DEFAULT_RANDOM_SEED,
        };

        let qsim = QSim {
            start_time: 0,
            end_time: 42,
            ticks_per_second: 1,
            sample_size: 0.1,
            stuck_threshold: 1,
            main_modes: vec!["bike".to_string()],
        };
        let controller = Controller {
            first_iteration: 2,
            last_iteration: 4,
            write_events_interval: 3,
            write_plans_interval: 5,
            compression_type: CompressionType::Zst,
        };

        config.set_partitioning(partitioning);
        config.set_computational_setup(computational_setup);
        config.set_qsim(qsim);
        config.set_controller(controller);

        let yaml = serde_yaml::to_string(&config).expect("Failed to serialize yaml");

        println!("{yaml}");

        let parsed_config: Config = serde_yaml::from_str(&yaml).expect("failed to parse config");
        println!("done.");

        assert_eq!(parsed_config.partitioning().num_parts, 1);
        assert_eq!(
            parsed_config.partitioning().method,
            PartitionMethod::Metis(MetisOptions {
                vertex_weight: vec![VertexWeight::InLinkCount, VertexWeight::InLinkCapacity],
                edge_weight: EdgeWeight::Constant,
                imbalance_factor: 1.02,
                iteration_number: 100,
                contiguous: true,
                min_conn: true,
            })
        );
        assert_eq!(
            parsed_config.partitioning().non_main_mode_links,
            NonMainModeLinks::Include
        );

        assert!(parsed_config.computational_setup().global_sync);
        assert_eq!(
            parsed_config.computational_setup().adapter_worker_threads,
            42
        );
        assert_eq!(parsed_config.computational_setup().replanning_threads, 7);
        assert_eq!(parsed_config.computational_setup().retry_time_seconds, 41);

        assert_eq!(parsed_config.controller().first_iteration, 2);
        assert_eq!(parsed_config.controller().last_iteration, 4);
        assert_eq!(parsed_config.controller().write_events_interval, 3);
        assert_eq!(parsed_config.controller().write_plans_interval, 5);
        assert_eq!(
            parsed_config.controller().compression_type,
            CompressionType::Zst
        );
        assert_eq!(parsed_config.qsim().start_time, 0);
        assert_eq!(parsed_config.qsim().end_time, 42);
        assert_eq!(parsed_config.qsim().ticks_per_second, 1);
        assert_eq!(parsed_config.qsim().sample_size, 0.1);
        assert_eq!(parsed_config.qsim().stuck_threshold, 1);
        assert_eq!(parsed_config.qsim().main_modes, vec!["bike"]);
    }

    #[test]
    fn controller_defaults_include_iteration_range_and_compression() {
        let config = Config::default();

        assert_eq!(config.controller().first_iteration, 0);
        assert_eq!(config.controller().last_iteration, 1000);
        assert_eq!(config.controller().write_events_interval, 50);
        assert_eq!(config.controller().write_plans_interval, 50);
        assert_eq!(config.controller().compression_type, CompressionType::Proto);
    }

    #[test]
    fn deprecated_simulation_module_migrates_to_qsim_and_controller() {
        let yaml = r#"
        modules:
          simulation:
            type: Simulation
            first_iteration: 2
            last_iteration: 4
            write_events_interval: 3
            write_plans_interval: 5
            start_time: 1
            end_time: 42
            ticks_per_second: 10
            sample_size: 0.5
            stuck_threshold: 99
            main_modes: ["car", "bike"]
        "#;

        let parsed_config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
        assert_eq!(parsed_config.controller().first_iteration, 2);
        assert_eq!(parsed_config.controller().last_iteration, 4);
        assert_eq!(parsed_config.controller().write_events_interval, 3);
        assert_eq!(parsed_config.controller().write_plans_interval, 5);
        assert_eq!(
            parsed_config.controller().compression_type,
            CompressionType::Proto
        );
        assert_eq!(parsed_config.qsim().start_time, 1);
        assert_eq!(parsed_config.qsim().end_time, 42);
        assert_eq!(parsed_config.qsim().ticks_per_second, 10);
        assert_eq!(parsed_config.qsim().sample_size, 0.5);
        assert_eq!(parsed_config.qsim().stuck_threshold, 99);
        assert_eq!(parsed_config.qsim().main_modes, vec!["car", "bike"]);
    }

    #[test]
    fn read_none_partitioning() {
        let yaml = r#"
        modules:
          partitioning:
            type: Partitioning
            num_parts: 1
            method: None
        "#;
        let parsed_config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
        assert_eq!(parsed_config.partitioning().num_parts, 1);
        assert_eq!(parsed_config.partitioning().method, PartitionMethod::None);
        assert_eq!(
            parsed_config.partitioning().non_main_mode_links,
            NonMainModeLinks::AssignAfterPartitioningToNearest
        );
    }

    #[test]
    fn read_non_main_mode_links() {
        let yaml = |value: &str| {
            format!(
                r#"
        modules:
          partitioning:
            type: Partitioning
            num_parts: 2
            method: None
            non_main_mode_links: {value}
        "#
            )
        };

        for (value, expected) in [
            (
                "AssignAfterPartitioningToNearest",
                NonMainModeLinks::AssignAfterPartitioningToNearest,
            ),
            ("Include", NonMainModeLinks::Include),
        ] {
            let parsed_config: Config =
                serde_yaml::from_str(&yaml(value)).expect("failed to parse config");
            assert_eq!(parsed_config.partitioning().non_main_mode_links, expected);
        }

        assert!(serde_yaml::from_str::<Config>(&yaml("Nearest")).is_err());
    }

    #[test]
    fn read_metis_partitioning() {
        let yaml = r#"
        modules:
          partitioning:
            type: Partitioning
            num_parts: 1
            method: !Metis
              vertex_weight:
              - InLinkCount
              imbalance_factor: 1.1
              edge_weight: Capacity
        "#;
        let parsed_config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
        assert_eq!(parsed_config.partitioning().num_parts, 1);
        assert_eq!(
            parsed_config.partitioning().method,
            PartitionMethod::Metis(MetisOptions {
                vertex_weight: vec![VertexWeight::InLinkCount],
                edge_weight: EdgeWeight::Capacity,
                imbalance_factor: 1.1,
                iteration_number: 100,
                contiguous: false,
                min_conn: true,
            })
        );
    }

    #[test]
    fn read_routing_modes_from_yaml() {
        let yaml = r#"
        modules:
          routing:
            type: Routing
            mode: UsePlans
            network_modes:
              - car
              - bike
            teleported_mode_params:
              - mode: walk
                beeline_distance_factor: 1.3
                teleported_mode_speed: 1.4
              - mode: pt
                beeline_distance_factor: 1.1
                teleported_mode_speed: 8.0
        "#;

        let parsed_config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");

        assert_eq!(parsed_config.routing().mode, RoutingMode::UsePlans);
        assert_eq!(parsed_config.routing().network_modes, vec!["car", "bike"]);
        assert_eq!(parsed_config.routing().access_egress_mode, "walk");
        assert_eq!(
            parsed_config.routing().teleported_mode_params,
            vec![
                TeleportedParams {
                    mode: "walk".to_string(),
                    beeline_distance_factor: 1.3,
                    teleported_mode_speed: 1.4,
                },
                TeleportedParams {
                    mode: "pt".to_string(),
                    beeline_distance_factor: 1.1,
                    teleported_mode_speed: 8.0,
                },
            ]
        );
    }

    #[test]
    fn routing_defaults_include_walk_access_egress_and_teleported_params() {
        let default_routing = Routing::default();
        assert_eq!(default_routing.access_egress_mode, "walk");
        assert_eq!(
            default_routing.teleported_mode_params,
            vec![TeleportedParams {
                mode: "walk".to_string(),
                beeline_distance_factor: 1.3,
                teleported_mode_speed: 3.0 / 3.6,
            }]
        );

        let default_config = Config::default();
        assert_eq!(default_config.routing().access_egress_mode, "walk");
        assert_eq!(
            default_config.routing().teleported_mode_params,
            vec![TeleportedParams {
                mode: "walk".to_string(),
                beeline_distance_factor: 1.3,
                teleported_mode_speed: 3.0 / 3.6,
            }]
        );

        let yaml = r#"
        modules:
          routing:
            type: Routing
            mode: UsePlans
        "#;

        let parsed_config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");

        assert_eq!(parsed_config.routing().mode, RoutingMode::UsePlans);
        assert!(parsed_config.routing().network_modes.is_empty());
        assert_eq!(parsed_config.routing().access_egress_mode, "walk");
        assert_eq!(
            parsed_config.routing().teleported_mode_params,
            vec![TeleportedParams {
                mode: "walk".to_string(),
                beeline_distance_factor: 1.3,
                teleported_mode_speed: 3.0 / 3.6,
            }]
        );
    }

    #[test]
    fn read_replanning_from_yaml() {
        let yaml = r#"
        modules:
          replanning:
            type: Replanning
            fraction_of_iterations_to_disable_innovation: 0.8
            max_agent_plan_memory: 7
            plan_selector_for_removal: BestScore
            strategy_settings:
              - name: ReRoute
                weight: 0.1
                subpopulation: person
              - name: BestScore
                weight: 0.9
                subpopulation: freight
        "#;

        let parsed_config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");

        assert_eq!(
            parsed_config.replanning(),
            &Replanning {
                fraction_of_iterations_to_disable_innovation: 0.8,
                max_agent_plan_memory: 7,
                plan_selector_for_removal: "BestScore".to_string(),
                strategy_settings: vec![
                    StrategySetting {
                        name: "ReRoute".to_string(),
                        weight: 0.1,
                        subpopulation: "person".to_string(),
                    },
                    StrategySetting {
                        name: "BestScore".to_string(),
                        weight: 0.9,
                        subpopulation: "freight".to_string(),
                    },
                ],
            }
        );
    }

    #[test]
    fn replanning_defaults_are_available_on_default_config() {
        let config = Config::default();

        assert_eq!(
            config.replanning(),
            &Replanning {
                fraction_of_iterations_to_disable_innovation: 1.0,
                max_agent_plan_memory: 5,
                plan_selector_for_removal: WORST_SCORE_STRATEGY_NAME.to_string(),
                strategy_settings: vec![StrategySetting {
                    name: KEEP_LAST_SELECTED_STRATEGY_NAME.to_string(),
                    weight: 1.0,
                    subpopulation: "person".to_string(),
                }],
            }
        );
    }

    #[test]
    fn scoring_yaml_roundtrip_preserves_parameters() {
        let yaml = r#"
        modules:
          scoring:
            type: Scoring
            mode: Disabled
            write_experienced_plans: true
            activity_params:
              - activity_type: home
                typical_duration_s: 43200.0
            mode_params:
              - mode: car
                marginal_utility_of_traveling: -0.001
                marginal_utility_of_distance: -0.002
                monetary_distance_cost_rate: -0.003
                constant: 1.5
                daily_money_constant: -2.5
                daily_utility_constant: 3.5
            agent_params:
              - subpopulation: freight
                late_arrival: -12.0
                early_departure: -6.0
                performing: 4.0
                waiting: -3.0
                marginal_utility_of_money: 2.0
                aborted_plan_score: -24.0
            fraction_of_iterations_to_start_score_msa: 0.75
        "#;

        let config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
        let expected = Scoring {
            mode: ScoringMode::Disabled,
            write_experienced_plans: true,
            activity_params: vec![ActivityParameter {
                activity_type: "home".to_string(),
                typical_duration_s: 43_200.0,
            }],
            mode_params: vec![ModeParameter {
                mode: "car".to_string(),
                marginal_utility_of_traveling: -0.001,
                marginal_utility_of_distance: -0.002,
                monetary_distance_cost_rate: -0.003,
                constant: 1.5,
                daily_money_constant: -2.5,
                daily_utility_constant: 3.5,
            }],
            agent_params: vec![AgentParameter {
                subpopulation: "freight".to_string(),
                late_arrival: -12.0,
                early_departure: -6.0,
                performing: 4.0,
                waiting: -3.0,
                marginal_utility_of_money: 2.0,
                aborted_plan_score: -24.0,
            }],
            fraction_of_iterations_to_start_score_msa: Some(0.75),
        };
        assert_eq!(config.scoring(), &expected);

        let serialized = serde_yaml::to_string(&config).expect("failed to serialize config");
        let roundtrip: Config =
            serde_yaml::from_str(&serialized).expect("failed to deserialize roundtrip config");
        assert_eq!(roundtrip.scoring(), &expected);
    }

    #[test]
    fn scoring_yaml_uses_defaults_for_missing_lists_and_agent_fields() {
        let yaml = r#"
        modules:
          scoring:
            type: Scoring
            agent_params:
              - {}
        "#;

        let config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");

        assert_eq!(config.scoring(), &Scoring::default());
    }

    #[test]
    fn routing_empty_teleported_params_use_default_walk() {
        let yaml = r#"
        modules:
          routing:
            type: Routing
            mode: UsePlans
            teleported_mode_params: []
        "#;

        let parsed_config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");

        assert_eq!(
            parsed_config.routing().teleported_mode_params,
            vec![TeleportedParams {
                mode: "walk".to_string(),
                beeline_distance_factor: 1.3,
                teleported_mode_speed: 3.0 / 3.6,
            }]
        );
    }

    #[test]
    fn routing_other_teleported_params_also_include_default_walk() {
        let yaml = r#"
        modules:
          routing:
            type: Routing
            mode: UsePlans
            teleported_mode_params:
              - mode: pt
                beeline_distance_factor: 1.1
                teleported_mode_speed: 8.0
        "#;

        let parsed_config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");

        assert_eq!(
            parsed_config.routing().teleported_mode_params,
            vec![
                TeleportedParams {
                    mode: "pt".to_string(),
                    beeline_distance_factor: 1.1,
                    teleported_mode_speed: 8.0,
                },
                TeleportedParams {
                    mode: "walk".to_string(),
                    beeline_distance_factor: 1.3,
                    teleported_mode_speed: 3.0 / 3.6,
                },
            ]
        );
    }

    #[test]
    fn routing_explicit_walk_params_replace_defaults_without_duplicates() {
        let yaml = r#"
        modules:
          routing:
            type: Routing
            mode: UsePlans
            teleported_mode_params:
              - mode: walk
                beeline_distance_factor: 1.1
                teleported_mode_speed: 1.4
              - mode: walk
                beeline_distance_factor: 1.2
                teleported_mode_speed: 1.5
        "#;

        let parsed_config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");

        assert_eq!(
            parsed_config.routing().teleported_mode_params,
            vec![TeleportedParams {
                mode: "walk".to_string(),
                beeline_distance_factor: 1.2,
                teleported_mode_speed: 1.5,
            }]
        );
    }

    #[test]
    fn test_imbalance_factor() {
        let ufactor = |factor| {
            MetisOptions::default()
                .set_imbalance_factor(factor)
                .ufactor()
        };
        assert_eq!(30, MetisOptions::default().ufactor());
        assert_eq!(30, ufactor(1.03));
        assert_eq!(100, ufactor(1.1));
        assert_eq!(1100, ufactor(2.1));
        assert_eq!(1, ufactor(1.001));
        assert_eq!(1, ufactor(1.0001));
        assert_eq!(1, ufactor(1.));
    }

    #[test]
    #[should_panic(expected = "imbalance_factor must be at least 1")]
    fn imbalance_factor_below_one_is_rejected() {
        MetisOptions::default().set_imbalance_factor(0.03).ufactor();
    }

    fn write_temp_config(yaml: &str) -> NamedTempFile {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(yaml.as_bytes()).unwrap();
        file
    }

    #[test]
    fn test_override_population_path() {
        let yaml = r#"
modules:
  population:
    type: Population
    path: pop
  output:
    type: Output
    output_dir: out
"#;

        let file = write_temp_config(yaml);

        let args = CommandLineArgs {
            config: file.path().to_str().unwrap().to_string(),
            overrides: vec![("population.path".to_string(), "new_pop".to_string())],
        };

        let config = Config::from_args(args);

        assert_eq!(
            config.population().path.as_ref().unwrap().to_str().unwrap(),
            "new_pop"
        );
    }

    #[test]
    fn test_optional_path() {
        let yaml = r#"
modules:
  population:
    type: Population
"#;
        let file = write_temp_config(yaml);
        let args = CommandLineArgs {
            config: file.path().to_str().unwrap().to_string(),
            overrides: vec![],
        };
        let config = Config::from_args(args);
        assert_eq!(config.population().path, None);
    }

    #[test]
    fn test_optional_path_null() {
        let yaml = r#"
modules:
  population:
    type: Population
    path: null
"#;
        let file = write_temp_config(yaml);
        let args = CommandLineArgs {
            config: file.path().to_str().unwrap().to_string(),
            overrides: vec![],
        };
        let config = Config::from_args(args);
        assert_eq!(config.population().path, None);
    }

    #[test]
    fn transit_defaults_to_no_schedule() {
        let config = Config::default();

        assert_eq!(None, config.transit().schedule_path);
        assert_eq!(None, config.transit().vehicles_path);
        assert_eq!(TransitSimulation::Teleported, config.transit().simulation);
        assert_eq!(vec!["pt".to_string()], config.transit().passenger_modes);
    }

    #[test]
    fn read_schedule_based_transit_from_yaml_with_default_passenger_modes() {
        let yaml = r#"
modules:
  transit:
    type: Transit
    schedule_path: schedule.xml.gz
    vehicles_path: transitVehicles.xml.gz
    simulation: ScheduleBased
"#;

        let parsed_config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");

        let transit = parsed_config.transit();
        assert_eq!(
            Some(PathBuf::from("transitVehicles.xml.gz")),
            transit.vehicles_path
        );
        assert_eq!(TransitSimulation::ScheduleBased, transit.simulation);
        assert_eq!(vec!["pt".to_string()], transit.passenger_modes);
    }

    #[test]
    fn override_transit_simulation_and_vehicles_path() {
        let mut config = base_config();
        config.apply_overrides(&[
            (
                "transit.simulation".to_string(),
                "schedule_based".to_string(),
            ),
            (
                "transit.vehicles_path".to_string(),
                "transitVehicles.xml".to_string(),
            ),
        ]);

        assert_eq!(
            TransitSimulation::ScheduleBased,
            config.transit().simulation
        );
        assert_eq!(
            Some(PathBuf::from("transitVehicles.xml")),
            config.transit().vehicles_path
        );
    }

    #[test]
    fn read_transit_schedule_path_from_yaml() {
        let yaml = r#"
modules:
  transit:
    type: Transit
    schedule_path: schedule.xml.gz
"#;

        let parsed_config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");

        assert_eq!(
            Some(PathBuf::from("schedule.xml.gz")),
            parsed_config.transit().schedule_path
        );
    }

    #[test]
    fn test_override_transit_schedule_path() {
        let yaml = r#"
modules:
  transit:
    type: Transit
    schedule_path: schedule.xml
"#;
        let file = write_temp_config(yaml);
        let args = CommandLineArgs {
            config: file.path().to_str().unwrap().to_string(),
            overrides: vec![(
                "transit.schedule_path".to_string(),
                "schedule.binpb".to_string(),
            )],
        };

        let config = Config::from_args(args);

        assert_eq!(
            Some(PathBuf::from("schedule.binpb")),
            config.transit().schedule_path
        );
    }

    #[test]
    fn facilities_defaults_to_no_path() {
        let config = Config::default();

        assert_eq!(None, config.facilities().path);
        assert_eq!(
            ModalLinkSelection::BaseLinkFirst,
            config.facilities().modal_link_selection
        );
    }

    #[test]
    fn read_modal_link_selection_from_yaml() {
        let yaml = r#"
modules:
  facilities:
    type: Facilities
    modal_link_selection: NearestLink
"#;

        let parsed_config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");

        assert_eq!(
            ModalLinkSelection::NearestLink,
            parsed_config.facilities().modal_link_selection
        );
    }

    #[test]
    fn test_override_modal_link_selection() {
        let yaml = r#"
modules:
  facilities:
    type: Facilities
"#;
        let file = write_temp_config(yaml);
        let args = CommandLineArgs {
            config: file.path().to_str().unwrap().to_string(),
            overrides: vec![(
                "facilities.modal_link_selection".to_string(),
                "nearest_link".to_string(),
            )],
        };

        let config = Config::from_args(args);

        assert_eq!(
            ModalLinkSelection::NearestLink,
            config.facilities().modal_link_selection
        );
    }

    #[test]
    fn test_override_facilities_path() {
        let yaml = r#"
modules:
  facilities:
    type: Facilities
    path: facilities.xml
"#;
        let file = write_temp_config(yaml);
        let args = CommandLineArgs {
            config: file.path().to_str().unwrap().to_string(),
            overrides: vec![(
                "facilities.path".to_string(),
                "facilities.binpb".to_string(),
            )],
        };

        let config = Config::from_args(args);

        assert_eq!(
            Some(PathBuf::from("facilities.binpb")),
            config.facilities().path
        );
    }

    #[test]
    fn test_override_output_dir() {
        let yaml = r#"
modules:
  output:
    type: Output
    output_dir: out
"#;
        let file = write_temp_config(yaml);
        let args = CommandLineArgs {
            config: file.path().to_str().unwrap().to_string(),
            overrides: vec![("output.output_dir".to_string(), "new_out".to_string())],
        };
        let config = Config::from_args(args);
        assert_eq!(config.output().output_dir.to_str().unwrap(), "new_out");
    }

    #[test]
    fn test_parse_output_overwrite_files() {
        let yaml = r#"
modules:
  output:
    type: Output
    output_dir: out
    overwrite_files: DeleteDirectoryIfExists
"#;
        let file = write_temp_config(yaml);
        let args = CommandLineArgs {
            config: file.path().to_str().unwrap().to_string(),
            overrides: vec![],
        };
        let config = Config::from_args(args);
        assert_eq!(
            config.output().overwrite_files,
            OverwriteFiles::DeleteDirectoryIfExists
        );
    }

    #[test]
    fn test_override_output_overwrite_files() {
        let yaml = r#"
modules:
  output:
    type: Output
    output_dir: out
"#;
        let file = write_temp_config(yaml);
        let args = CommandLineArgs {
            config: file.path().to_str().unwrap().to_string(),
            overrides: vec![(
                "output.overwrite_files".to_string(),
                "FailIfDirectoryExists".to_string(),
            )],
        };
        let config = Config::from_args(args);
        assert_eq!(
            config.output().overwrite_files,
            OverwriteFiles::FailIfDirectoryExists
        );
    }

    #[test]
    fn test_override_partitioning_num_parts() {
        let yaml = r#"
modules:
  partitioning:
    type: Partitioning
    num_parts: 1
    method: None
  output:
    type: Output
    output_dir: out
"#;
        let file = write_temp_config(yaml);
        let args = CommandLineArgs {
            config: file.path().to_str().unwrap().to_string(),
            overrides: vec![("partitioning.num_parts".to_string(), "5".to_string())],
        };
        let config = Config::from_args(args);
        assert_eq!(config.partitioning().num_parts, 5);
    }

    #[test]
    fn test_parse_key_val_valid() {
        let input = "population.path=some_path";
        let parsed = parse_key_val(input);
        assert_eq!(
            parsed,
            Ok(("population.path".to_string(), "some_path".to_string()))
        );
    }

    #[test]
    fn test_parse_key_val_invalid() {
        let input = "population.path_some_path";
        let parsed = parse_key_val(input);
        assert!(parsed.is_err());
    }

    fn base_config() -> Config {
        let mut config = Config::default();

        config.set_network(Network {
            path: Some("net".into()),
        });
        config.set_population(Population {
            path: Some("pop".into()),
        });
        config.set_vehicles(Vehicles {
            path: Some("veh".into()),
        });
        config.set_transit(Transit {
            schedule_path: Some("schedule".into()),
            ..Transit::default()
        });
        config.set_ids(Ids {
            path: Some("ids".into()),
        });

        config.set_output(Output {
            output_dir: "out".into(),
            overwrite_files: OverwriteFiles::OverwriteExistingFiles,
            profiling: Profiling::None,
            logging: Logging::Info,
            write_events: WriteEvents::None,
        });
        config.set_partitioning(Partitioning {
            num_parts: 1,
            method: PartitionMethod::None,
            non_main_mode_links: NonMainModeLinks::default(),
        });
        config.set_routing(Routing {
            mode: RoutingMode::UsePlans,
            network_modes: Vec::new(),
            access_egress_mode: "walk".to_string(),
            teleported_mode_params: vec![TeleportedParams {
                mode: "walk".to_string(),
                beeline_distance_factor: 1.3,
                teleported_mode_speed: 3.0 / 3.6,
            }],
        });
        config
    }

    #[test]
    fn override_network_path() {
        let mut config = base_config();
        config.apply_overrides(&[("network.path".to_string(), "new_net".to_string())]);
        assert_eq!(config.network().path, Some(PathBuf::from("new_net")));
    }

    #[test]
    fn override_partitioning_num_parts() {
        let mut config = base_config();
        config.apply_overrides(&[("partitioning.num_parts".to_string(), "7".to_string())]);
        assert_eq!(config.partitioning().num_parts, 7);
    }

    #[test]
    fn override_replanning_threads() {
        let mut config = base_config();
        config.apply_overrides(&[(
            "computational_setup.replanning_threads".to_string(),
            "3".to_string(),
        )]);
        assert_eq!(config.computational_setup().replanning_threads, 3);
    }

    #[test]
    fn override_controller_and_qsim_settings() {
        let mut config = base_config();
        config.apply_overrides(&[
            ("controller.first_iteration".to_string(), "12".to_string()),
            ("controller.last_iteration".to_string(), "34".to_string()),
            (
                "controller.write_events_interval".to_string(),
                "7".to_string(),
            ),
            (
                "controller.write_plans_interval".to_string(),
                "9".to_string(),
            ),
            ("controller.compression_type".to_string(), "zst".to_string()),
            ("qsim.start_time".to_string(), "1".to_string()),
            ("qsim.end_time".to_string(), "2".to_string()),
            ("qsim.ticks_per_second".to_string(), "10".to_string()),
            ("qsim.sample_size".to_string(), "0.25".to_string()),
            ("qsim.stuck_threshold".to_string(), "30".to_string()),
            ("qsim.main_modes".to_string(), "car,bike".to_string()),
        ]);

        assert_eq!(config.controller().first_iteration, 12);
        assert_eq!(config.controller().last_iteration, 34);
        assert_eq!(config.controller().write_events_interval, 7);
        assert_eq!(config.controller().write_plans_interval, 9);
        assert_eq!(config.controller().compression_type, CompressionType::Zst);
        assert_eq!(config.qsim().start_time, 1);
        assert_eq!(config.qsim().end_time, 2);
        assert_eq!(config.qsim().ticks_per_second, 10);
        assert_eq!(config.qsim().sample_size, 0.25);
        assert_eq!(config.qsim().stuck_threshold, 30);
        assert_eq!(config.qsim().main_modes, vec!["car", "bike"]);
    }

    #[test]
    fn override_write_experienced_plans() {
        let mut config = base_config();
        config.apply_overrides(&[(
            "scoring.write_experienced_plans".to_string(),
            "false".to_string(),
        )]);

        assert!(!config.scoring().write_experienced_plans);
    }

    #[test]
    fn override_scoring_mode() {
        let mut config = base_config();
        assert_eq!(config.scoring().mode, ScoringMode::Enabled);

        config.apply_overrides(&[("scoring.mode".to_string(), "disabled".to_string())]);

        assert_eq!(config.scoring().mode, ScoringMode::Disabled);
    }

    #[test]
    fn override_fraction_of_iterations_to_start_score_msa() {
        let mut config = base_config();
        assert_eq!(
            config.scoring().fraction_of_iterations_to_start_score_msa,
            None
        );

        config.apply_overrides(&[(
            "scoring.fraction_of_iterations_to_start_score_msa".to_string(),
            "0.9".to_string(),
        )]);

        assert_eq!(
            config.scoring().fraction_of_iterations_to_start_score_msa,
            Some(0.9)
        );
    }

    #[test]
    fn override_write_events() {
        let mut config = base_config();
        assert_eq!(config.output().write_events, WriteEvents::None);

        config.apply_overrides(&[("output.write_events".to_string(), "File".to_string())]);
        assert_eq!(config.output().write_events, WriteEvents::File);

        config.apply_overrides(&[("output.write_events".to_string(), "none".to_string())]);
        assert_eq!(config.output().write_events, WriteEvents::None);
    }

    #[test]
    fn override_partitioning_method() {
        let mut config = base_config();
        config.partitioning_mut().method = PartitionMethod::Metis(MetisOptions::default());

        config.apply_overrides(&[("partitioning.method".to_string(), "none".to_string())]);

        assert_eq!(config.partitioning().method, PartitionMethod::None);
    }

    #[test]
    fn override_routing_mode() {
        let mut config = base_config();
        config.apply_overrides(&[("routing.mode".to_string(), "ad-hoc".to_string())]);
        assert_eq!(config.routing().mode, RoutingMode::AdHoc);
    }

    #[test]
    #[should_panic]
    fn override_routing_mode_invalid() {
        let mut config = base_config();
        config.apply_overrides(&[("routing.mode".to_string(), "InvalidMode".to_string())]);
    }

    #[test]
    fn travel_time_calculator_yaml_roundtrip_preserves_explicit_horizon() {
        let yaml = r#"
modules:
  qsim:
    type: QSim
    end_time: 42
  travel_time_calculator:
    type: TravelTimeCalculator
    bin_size: 300
"#;
        let config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
        // Integer values from existing configs remain valid seconds.
        assert_eq!(300.0, config.travel_time_calculator().bin_size);

        let serialized = serde_yaml::to_string(&config).expect("failed to serialize config");
        let roundtrip: Config =
            serde_yaml::from_str(&serialized).expect("failed to deserialize roundtrip config");
        assert_eq!(
            config.travel_time_calculator(),
            roundtrip.travel_time_calculator()
        );
    }

    #[test]
    fn travel_time_calculator_yaml_roundtrip_preserves_fractional_bin_size() {
        let yaml = r#"
modules:
  travel_time_calculator:
    type: TravelTimeCalculator
    bin_size: 0.5
"#;
        let config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
        assert_eq!(0.5, config.travel_time_calculator().bin_size);

        let serialized = serde_yaml::to_string(&config).expect("failed to serialize config");
        let roundtrip: Config =
            serde_yaml::from_str(&serialized).expect("failed to deserialize roundtrip config");
        assert_eq!(
            config.travel_time_calculator(),
            roundtrip.travel_time_calculator()
        );
    }

    #[test]
    fn travel_time_calculator_overrides_are_applied() {
        let mut config = Config::default();
        config.apply_overrides(&[(
            "travel_time_calculator.bin_size".to_string(),
            "60".to_string(),
        )]);

        assert_eq!(
            &TravelTimeCalculator { bin_size: 60.0 },
            config.travel_time_calculator()
        );

        config.apply_overrides(&[(
            "travel_time_calculator.bin_size".to_string(),
            "0.25".to_string(),
        )]);

        assert_eq!(
            &TravelTimeCalculator { bin_size: 0.25 },
            config.travel_time_calculator()
        );
    }

    #[test]
    fn travel_time_calculator_default_bin_size_is_independent_of_ticks() {
        let calculator = TravelTimeCalculator::default();

        assert_eq!(Ok(Duration::from_secs(900)), calculator.bin_duration(1));
        assert_eq!(Ok(Duration::from_secs(900)), calculator.bin_duration(10));
    }

    #[test]
    fn travel_time_calculator_accepts_whole_multiples_of_the_tick_length() {
        for (bin_size, ticks_per_second, expected) in [
            (900.0, 1, Duration::from_secs(900)),
            (0.5, 2, Duration::from_millis(500)),
            (0.5, 10, Duration::from_millis(500)),
            // Not exactly representable in binary, but exactly one or several ticks.
            (0.1, 10, Duration::from_millis(100)),
            (0.3, 10, Duration::from_millis(300)),
            (0.7, 10, Duration::from_millis(700)),
            (2.5, 4, Duration::from_millis(2500)),
            // The tick length of 1/3 s has no exact nanosecond value, but whole seconds do.
            (1.0, 3, Duration::from_secs(1)),
        ] {
            let calculator = TravelTimeCalculator { bin_size };
            assert_eq!(
                Ok(expected),
                calculator.bin_duration(ticks_per_second),
                "bin_size {bin_size} with {ticks_per_second} ticks per second"
            );
        }
    }

    #[test]
    fn travel_time_calculator_rejects_fractions_of_a_tick() {
        for (bin_size, ticks_per_second) in [(0.5, 1), (900.5, 1), (0.25, 10), (0.05, 10)] {
            let calculator = TravelTimeCalculator { bin_size };
            assert_eq!(
                Err(format!(
                    "travel_time_calculator.bin_size ({bin_size} s) must be a whole multiple of the tick length (1/{ticks_per_second} s)"
                )),
                calculator.bin_duration(ticks_per_second)
            );
        }
    }

    #[test]
    fn travel_time_calculator_rejects_bins_without_exact_nanosecond_width() {
        let calculator = TravelTimeCalculator {
            bin_size: 1.0 / 3.0,
        };

        assert_eq!(
            Err(format!(
                "travel_time_calculator.bin_size ({} s) cannot be represented in whole nanoseconds with qsim.ticks_per_second = 3",
                1.0 / 3.0
            )),
            calculator.bin_duration(3)
        );
    }

    #[test]
    fn travel_time_calculator_rejects_zero_bin_size() {
        let calculator = TravelTimeCalculator { bin_size: 0.0 };

        assert_eq!(
            Err(
                "travel_time_calculator.bin_size must be finite and greater than 0, got 0"
                    .to_string()
            ),
            calculator.bin_duration(1)
        );
    }

    #[test]
    fn travel_time_calculator_rejects_negative_and_non_finite_bin_sizes() {
        for bin_size in [-900.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let calculator = TravelTimeCalculator { bin_size };
            assert_eq!(
                Err(format!(
                    "travel_time_calculator.bin_size must be finite and greater than 0, got {bin_size}"
                )),
                calculator.bin_duration(1)
            );
        }
    }

    #[test]
    fn travel_time_calculator_rejects_bins_beyond_the_simulation_time_range() {
        for (bin_size, ticks_per_second) in [(1e11, 1), (1e30, 7), (f64::MAX, u32::MAX)] {
            let calculator = TravelTimeCalculator { bin_size };
            assert_eq!(
                Err(format!(
                    "travel_time_calculator.bin_size ({bin_size} s) exceeds the supported time range"
                )),
                calculator.bin_duration(ticks_per_second)
            );
        }
    }

    #[test]
    fn travel_time_calculator_rejects_zero_ticks_per_second() {
        assert_eq!(
            Err("qsim.ticks_per_second must be greater than 0".to_string()),
            TravelTimeCalculator::default().bin_duration(0)
        );
    }
}
