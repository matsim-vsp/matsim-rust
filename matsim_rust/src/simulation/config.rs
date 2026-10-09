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
use std::collections::BTreeMap;
use std::fmt::Debug;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter};
use std::path::{Path, PathBuf};
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
#[command(author, version, about, long_about = None)]
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
    /// Drive the schedule's vehicles on the network and let passengers board them, instead of
    /// teleporting PT legs. MATSim's `transit.usingTransitInMobsim`.
    #[serde(default)]
    pub simulate_vehicles: bool,
    /// Leg modes served by simulated transit vehicles. MATSim's `transit.transitModes`.
    #[serde(default = "default_transit_modes")]
    pub transit_modes: Vec<String>,
    /// Use service-to-passenger mode mappings for transit routing, scoring, and returned ride legs.
    #[serde(default)]
    pub use_mode_mapping_for_passengers: bool,
    /// Map schedule route modes to the passenger leg modes used by MATSim's SwissRailRaptor.
    #[serde(default)]
    pub mode_mapping_for_passengers: BTreeMap<String, String>,
    /// Let `pt` requests that carry no person fall back to the car router. This is the legacy
    /// behaviour SILO's zone-to-zone queries rely on; passengers never need it, since a
    /// passenger's car fallback is gated on that agent's `ownsCar` attribute.
    #[serde(default)]
    pub personless_car_fallback: bool,
    #[serde(default)]
    pub use_intermodal_access_egress: bool,
    #[serde(default)]
    pub intermodal_access_egress: Vec<IntermodalAccessEgress>,
    #[serde(default)]
    pub intermodal_access_egress_mode_selection: IntermodalModeSelection,
    #[serde(default)]
    pub intermodal_leg_only_handling: IntermodalLegOnlyHandling,
    /// When walking transfer candidates are built for transit routing.
    #[serde(default)]
    pub transfer_construction: TransferConstruction,
    /// Search for PT routes within configured departure windows. Empty means use the desired
    /// departure time only; an empty subpopulation list applies to every subpopulation.
    #[serde(default)]
    pub range_query_settings: Vec<TransitRangeQuerySettings>,
    /// Route choice weights for window searches. The default score is travel time plus 300
    /// seconds per transfer, matching the existing pinned router cost.
    #[serde(default)]
    pub route_selector_settings: Vec<TransitRouteSelectorSettings>,
    /// Transfer penalties applied to every transfer of a PT itinerary. MATSim's
    /// `transferPenaltyBaseCost`, `transferPenaltyCostPerTravelTimeHour`,
    /// `transferPenaltyMinCost` and `transferPenaltyMaxCost`.
    #[serde(default)]
    pub transfer_penalty: TransitTransferPenalty,
}

/// Transfer penalties in utils, mirroring MATSim's `RaptorParameters` transfer costs.
///
/// `per_travel_time_hour` follows MATSim's backwards-compatibility switch: while it is zero the
/// configured base cost is ignored and the pinned default utility of a line switch applies
/// instead, so a config that sets only `base_cost` behaves like MATSim rather than silently
/// changing cost. `min_cost` and `max_cost` bound one transfer's penalty; the defaults are
/// unbounded, as in MATSim.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct TransitTransferPenalty {
    /// Utility cost of one transfer. MATSim's `transferPenaltyBaseCost`.
    pub base_cost: f64,
    /// Utility cost per hour of the journey so far, charged for every transfer. MATSim's
    /// `transferPenaltyCostPerTravelTimeHour`.
    pub per_travel_time_hour: f64,
    /// Lower bound on one transfer's penalty. MATSim's `transferPenaltyMinCost`.
    pub min_cost: f64,
    /// Upper bound on one transfer's penalty. MATSim's `transferPenaltyMaxCost`.
    pub max_cost: f64,
    /// Additional penalty per transport-mode pair. Setting any of these switches the router to
    /// MATSim's `ModeSpecificTransferCostCalculator`, which cannot combine them with a
    /// per-travel-time-hour cost. MATSim's `modeToModeTransferPenalty` parametersets.
    pub by_transport_mode: Vec<TransitModeToModeTransferPenalty>,
}

/// MATSim's `modeToModeTransferPenalty` parameterset: an extra penalty for transferring between
/// two transport modes.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct TransitModeToModeTransferPenalty {
    pub from_mode: String,
    pub to_mode: String,
    pub transfer_penalty: f64,
}

impl Default for TransitTransferPenalty {
    fn default() -> Self {
        Self {
            base_cost: 0.0,
            per_travel_time_hour: 0.0,
            min_cost: f64::NEG_INFINITY,
            max_cost: f64::INFINITY,
            by_transport_mode: Vec::new(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TransferConstruction {
    /// Build all candidate stop transfers when the router is created.
    #[default]
    Initial,
    /// Build and cache candidates the first time each stop is queried.
    Adaptive,
    /// Rebuild candidates on every query without retaining them.
    Online,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct IntermodalAccessEgress {
    pub mode: String,
    pub initial_search_radius: f64,
    pub max_radius: f64,
    pub search_extension_radius: f64,
    pub share_trip_search_radius: f64,
    pub person_filter_attribute: Option<String>,
    pub person_filter_value: Option<String>,
    pub stop_filter_attribute: Option<String>,
    pub stop_filter_value: Option<String>,
    pub link_id_attribute: Option<String>,
}

impl Default for IntermodalAccessEgress {
    fn default() -> Self {
        Self {
            mode: String::new(),
            initial_search_radius: 1_000.0,
            max_radius: f64::INFINITY,
            search_extension_radius: 500.0,
            share_trip_search_radius: f64::INFINITY,
            person_filter_attribute: None,
            person_filter_value: None,
            stop_filter_attribute: None,
            stop_filter_value: None,
            link_id_attribute: None,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct TransitRangeQuerySettings {
    pub max_earlier_departure_sec: u64,
    pub max_later_departure_sec: u64,
    pub subpopulations: Vec<String>,
}

impl Default for TransitRangeQuerySettings {
    fn default() -> Self {
        Self {
            max_earlier_departure_sec: 0,
            max_later_departure_sec: 0,
            subpopulations: Vec::new(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IntermodalModeSelection {
    #[default]
    LeastCostPerStop,
    RandomPerDirection,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IntermodalLegOnlyHandling {
    Allow,
    Avoid,
    #[default]
    Forbid,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct TransitRouteSelectorSettings {
    pub beta_travel_time: f64,
    pub beta_departure_time: f64,
    pub beta_transfer_count: f64,
    pub subpopulations: Vec<String>,
}

impl Default for TransitRouteSelectorSettings {
    fn default() -> Self {
        Self {
            beta_travel_time: 1.0,
            beta_departure_time: 0.0,
            beta_transfer_count: 300.0,
            subpopulations: Vec::new(),
        }
    }
}

fn default_transit_modes() -> Vec<String> {
    vec!["pt".to_string()]
}

impl Default for Transit {
    fn default() -> Self {
        Self {
            schedule_path: None,
            simulate_vehicles: false,
            transit_modes: default_transit_modes(),
            use_mode_mapping_for_passengers: false,
            mode_mapping_for_passengers: BTreeMap::new(),
            personless_car_fallback: false,
            use_intermodal_access_egress: false,
            intermodal_access_egress: Vec::new(),
            intermodal_access_egress_mode_selection: IntermodalModeSelection::default(),
            intermodal_leg_only_handling: IntermodalLegOnlyHandling::default(),
            transfer_construction: TransferConstruction::default(),
            range_query_settings: Vec::new(),
            route_selector_settings: Vec::new(),
            transfer_penalty: TransitTransferPenalty::default(),
        }
    }
}

impl Transit {
    pub fn validate(&self) -> Result<(), String> {
        for (index, selector) in self.route_selector_settings.iter().enumerate() {
            if !selector.beta_travel_time.is_finite()
                || !selector.beta_departure_time.is_finite()
                || !selector.beta_transfer_count.is_finite()
            {
                return Err(format!(
                    "transit.route_selector_settings[{index}] weights must be finite"
                ));
            }
        }
        self.transfer_penalty.validate()
    }
}

impl TransitTransferPenalty {
    /// The pinned default utility of a line switch, i.e. `-utilityOfLineSwitch` at MATSim's
    /// default of -1 utils. MATSim applies it instead of the configured base cost while no
    /// per-travel-time-hour cost is configured.
    pub const DEFAULT_LINE_SWITCH_COST: f64 = 1.0;

    pub fn validate(&self) -> Result<(), String> {
        for (name, value) in [
            ("base_cost", self.base_cost),
            ("per_travel_time_hour", self.per_travel_time_hour),
        ] {
            if !value.is_finite() {
                return Err(format!("transit.transfer_penalty.{name} must be finite"));
            }
        }
        // The bounds default to MATSim's unbounded limits, so an infinite bound is a deliberate
        // "unbounded" and only a NaN is rejected outright.
        if self.min_cost.is_nan() || self.max_cost.is_nan() {
            return Err("transit.transfer_penalty bounds must not be NaN".to_string());
        }

        // A zero-width interval pins every transfer to one value, so an infinite one would make
        // `clip` return an infinity that then flows into route costs. `min > max` does not catch
        // it, because `inf > inf` is false. An interval that is only unbounded on one side, which
        // includes the pinned defaults, is fine.
        if self.min_cost == self.max_cost && !self.min_cost.is_finite() {
            return Err(format!(
                "transit.transfer_penalty bounds pin every transfer to {}, which must be finite",
                self.min_cost
            ));
        }
        for (index, penalty) in self.by_transport_mode.iter().enumerate() {
            if !penalty.transfer_penalty.is_finite() {
                return Err(format!(
                    "transit.transfer_penalty.by_transport_mode[{index}].transfer_penalty must be \
                     finite"
                ));
            }
            if penalty.from_mode.is_empty() || penalty.to_mode.is_empty() {
                return Err(format!(
                    "transit.transfer_penalty.by_transport_mode[{index}] must name both modes"
                ));
            }
        }
        if !self.by_transport_mode.is_empty() && self.per_travel_time_hour != 0.0 {
            // MATSim's ModeSpecificTransferCostCalculator rejects the combination outright, so a
            // config asking for both fails here instead of silently dropping one.
            return Err(
                "transit.transfer_penalty.by_transport_mode cannot be combined with \
                 per_travel_time_hour"
                    .to_string(),
            );
        }
        Ok(())
    }

    /// The utility charged for one transfer, before any mode-specific offset. MATSim's
    /// `RaptorUtils.createParameters`: while no hourly cost is configured the pinned line
    /// switch utility replaces the configured base cost.
    pub fn base_cost(&self) -> f64 {
        if self.per_travel_time_hour == 0.0 {
            return Self::DEFAULT_LINE_SWITCH_COST;
        }
        self.base_cost
    }

    /// Whether any mode pair carries an extra penalty, which selects MATSim's
    /// `ModeSpecificTransferCostCalculator`.
    pub fn is_mode_specific(&self) -> bool {
        !self.by_transport_mode.is_empty()
    }

    /// The extra penalty for transferring between two transport modes; zero when unconfigured.
    pub fn mode_penalty(&self, from_mode: &str, to_mode: &str) -> f64 {
        self.by_transport_mode
            .iter()
            .find(|penalty| penalty.from_mode == from_mode && penalty.to_mode == to_mode)
            .map_or(0.0, |penalty| penalty.transfer_penalty)
    }

    /// One transfer's penalty in utils, clipped into the configured bounds. MATSim's
    /// `calcSingleTransferCost` normalizes the bounds with `Math.min`/`Math.max` before comparing,
    /// so an inverted pair behaves as a swap rather than being rejected.
    pub fn clip(&self, cost: f64) -> f64 {
        let min = self.min_cost.min(self.max_cost);
        let max = self.min_cost.max(self.max_cost);
        cost.clamp(min, max)
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

register_override!("transit.simulate_vehicles", |config, value| {
    config.transit_mut().simulate_vehicles = value.parse().unwrap();
});

register_override!("transit.transit_modes", |config, value| {
    config.transit_mut().transit_modes = value
        .split(',')
        .map(str::trim)
        .filter(|mode| !mode.is_empty())
        .map(ToString::to_string)
        .collect();
});

register_override!(
    "transit.use_mode_mapping_for_passengers",
    |config, value| {
        config.transit_mut().use_mode_mapping_for_passengers = value.parse().unwrap();
    }
);

register_override!("transit.personless_car_fallback", |config, value| {
    config.transit_mut().personless_car_fallback = value.parse().unwrap();
});

register_override!("transit.use_intermodal_access_egress", |config, value| {
    config.transit_mut().use_intermodal_access_egress = value.parse().unwrap();
});

register_override!(
    "transit.intermodal_access_egress_mode_selection",
    |config, value| {
        config.transit_mut().intermodal_access_egress_mode_selection = match value {
            "least_cost_per_stop" => IntermodalModeSelection::LeastCostPerStop,
            "random_per_direction" => IntermodalModeSelection::RandomPerDirection,
            _ => panic!("Invalid intermodal access/egress mode selection: {value}"),
        };
    }
);

register_override!("transit.intermodal_leg_only_handling", |config, value| {
    config.transit_mut().intermodal_leg_only_handling = match value {
        "allow" => IntermodalLegOnlyHandling::Allow,
        "avoid" => IntermodalLegOnlyHandling::Avoid,
        "forbid" => IntermodalLegOnlyHandling::Forbid,
        _ => panic!("Invalid intermodal leg-only handling: {value}"),
    };
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
}

register_override!("partitioning.num_parts", |config, value| {
    if let Ok(v) = value.parse() {
        config.partitioning_mut().num_parts = v;
        // replace some configuration if we get a partition from the outside. This is interesting for testing
        let out_dir = format!("{}-{v}", config.output().output_dir.to_str().unwrap());
        config.output_mut().output_dir = out_dir.into();
    }
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
    #[serde(default)]
    pub analysis: Analysis,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Analysis {
    pub enabled: bool,
    /// Width of exported link-volume intervals in seconds.
    pub interval_seconds: u32,
    /// Explicit per-link labels. Missing or blank labels are reported as `unknown`.
    pub link_labels: std::collections::BTreeMap<String, LinkLabels>,
    /// Optional polygon in the same coordinate system as network node coordinates.
    /// A link is inner when both endpoints are inside, outer when both are outside
    /// and the segment misses the polygon, and cross_boundary when one endpoint is
    /// inside or the segment crosses the polygon. When set, this determines
    /// `urban_area` instead of the per-link label of the same name.
    pub urban_boundary: Option<Vec<[f64; 2]>>,

    /// Optional observed-data CSV used by the validation report. Relative paths are resolved
    /// against the configured output directory.
    pub observed_data: Option<PathBuf>,
    /// Optional weighted journey records from a comparable travel survey.
    pub journey_survey: Option<PathBuf>,
    /// Run directories whose latest published analysis reports are included in a comparison.
    pub comparison_runs: Vec<PathBuf>,
    /// Optional DRT/taxi service records analysed after the run.
    pub service: Option<ServiceInputs>,
    /// Optional CSV of observed boardings and alightings for the transit validation. Relative
    /// paths are resolved against the configured output directory.
    pub transit_observed_data: Option<PathBuf>,
    /// Optional CSV of supplied utility and monetary appraisal inputs.
    pub economic_inputs: Option<PathBuf>,

    /// Optional modeled emission-event records CSV. Relative paths resolve from the output dir.
    pub emissions: Option<EmissionsInputs>,
    /// Optional modeled receiver sound/exposure records and affected population data.
    pub noise: Option<NoiseInputs>,

    /// Optional upper bound, in seconds, applied to positive free-flow-relative delay totals.
    pub excess_delay_clip_seconds: Option<f64>,

    /// Zone system the geographic reports are built from.
    pub zone_system: ZoneSystem,
    /// Reachability of supplied opportunities from the run's zones and people.
    pub accessibility: Accessibility,
    /// Person attributes the demographic module groups people by, in report order.
    pub person_group_attributes: Vec<String>,
    /// Person attribute holding a person's weight.
    pub person_weight_attribute: Option<String>,
    /// Person attribute holding a monetary travel cost for the day.
    pub person_cost_attribute: Option<String>,
}

/// Zone system supplied to the analysis, keyed by the external identifiers the recorded
/// events carry.
///
/// Two independent geographies are accepted, because a location can be described by either:
/// `link_zones` locates the links that journey origins, journey destinations and activities
/// are reported on, and `person_zones` locates the people themselves. A location without an
/// entry is reported as `unmapped` rather than dropped, so a partial zone system still
/// accounts for every observed trip.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct ZoneSystem {
    /// Identifier of the supplied zone system, recorded in the manifest and the report so a
    /// reader knows which definition the reported zones came from.
    pub name: Option<String>,
    /// External link ID to zone ID.
    pub link_zones: std::collections::BTreeMap<String, String>,
    /// External person ID to zone ID.
    pub person_zones: std::collections::BTreeMap<String, String>,
}

impl ZoneSystem {
    /// A zone system only becomes usable once it maps at least one location; an empty one
    /// leaves the geographic modules unavailable instead of reporting a single `unmapped` zone.
    pub fn is_empty(&self) -> bool {
        self.link_zones.is_empty() && self.person_zones.is_empty()
    }
}

/// Inputs of the accessibility-to-opportunities measure, and the thresholds it reports.
///
/// The three files form one input: the module stays unavailable until all of them are
/// configured, and configuring only some of them is an error rather than a partial
/// measure. Relative paths are resolved against the configured output directory.
///
/// Accessibility is defined on *potential* destinations, so `travel_costs` has to be a
/// supplied cost table. A realized trip duration says how long one person actually
/// took, which says nothing about how long anyone else *could* take, so observed
/// journey tables are never a substitute for these costs.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Accessibility {
    /// Opportunity locations, one row per location, with its weight and category.
    pub opportunities: Option<PathBuf>,
    /// Zone centroids. They are the explicit coordinate/zone correspondence used to
    /// place both opportunity locations and person home locations.
    pub zones: Option<PathBuf>,
    /// Potential-destination travel costs between zones, by mode and departure period.
    pub travel_costs: Option<PathBuf>,
    /// Cumulative-opportunity thresholds in seconds. Must be non-empty, and every entry
    /// must be finite and non-negative.
    pub thresholds_seconds: Vec<f64>,
}

impl Accessibility {
    /// The three files are one input, so a partially configured module cannot run.
    pub fn is_configured(&self) -> bool {
        self.opportunities.is_some() || self.zones.is_some() || self.travel_costs.is_some()
    }

    /// `Ok` when the configured set can produce a measure, otherwise the reason it cannot.
    pub fn validate(&self) -> Result<(), String> {
        let missing: Vec<&str> = [
            ("opportunities", self.opportunities.is_none()),
            ("zones", self.zones.is_none()),
            ("travel_costs", self.travel_costs.is_none()),
        ]
        .into_iter()
        .filter_map(|(name, absent)| absent.then_some(name))
        .collect();
        if !missing.is_empty() {
            return Err(format!(
                "output.analysis.accessibility needs {}, because all three inputs are required together",
                missing.join(", ")
            ));
        }
        if self.thresholds_seconds.is_empty() {
            return Err(
                "output.analysis.accessibility.thresholds_seconds must not be empty".to_owned(),
            );
        }
        if let Some(threshold) = self
            .thresholds_seconds
            .iter()
            .find(|threshold| !threshold.is_finite() || **threshold < 0.0)
        {
            return Err(format!(
                "output.analysis.accessibility.thresholds_seconds must hold non-negative finite values, got {threshold}"
            ));
        }
        Ok(())
    }
}

impl Default for Accessibility {
    fn default() -> Self {
        Self {
            opportunities: None,
            zones: None,
            travel_costs: None,
            // 45 minutes is the conventional accessibility cutoff, so a run that supplies
            // the three files without naming a threshold still reports a usable measure.
            thresholds_seconds: default_accessibility_thresholds_seconds(),
        }
    }
}

fn default_accessibility_thresholds_seconds() -> Vec<f64> {
    vec![2700.0]
}

/// Supplied records for DRT and taxi service performance. The analysis only reads them; no
/// service is simulated. Relative paths are resolved against the configured output directory.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ServiceInputs {
    /// One row per request. This is the only source of rejections.
    pub requests: PathBuf,
    /// Request-to-vehicle association records with pickup and drop-off times.
    pub passengers: Option<PathBuf>,
    /// Vehicle capacity and service window records.
    pub fleet: Option<PathBuf>,
    /// Vehicle task records with drive distances.
    pub schedule: Option<PathBuf>,
    /// Service area polygon in network node coordinates.
    #[serde(default)]
    pub service_area: Option<Vec<[f64; 2]>>,
    /// Configured maximum wait between request submission and pickup.
    #[serde(default)]
    pub max_wait_seconds: Option<f64>,
}

/// Supplied modeled emissions and the provenance needed to interpret their totals.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct EmissionsInputs {
    pub records: PathBuf,
    /// Category labels keyed by the vehicle type ID found in the run's vehicle catalog.
    pub vehicle_categories: std::collections::BTreeMap<String, String>,
    pub fleet_provenance: String,
    pub emission_factor_provenance: String,
    pub accounting_boundary: String,
}
/// Supplied noise model outputs. Analysis never invents receiver locations or exposure.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct NoiseInputs {
    /// Receiver/time records with receiver_id, period_start_seconds, period_end_seconds,
    /// metric, unit and value columns. Sound levels use dB and energy averaging.
    pub records: PathBuf,
    /// Optional receiver/time affected-population rows.
    #[serde(default)]
    pub affected_population: Option<PathBuf>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct LinkLabels {
    pub urban_area: Option<String>,
    pub road_type: Option<String>,
    pub road_size: Option<String>,
}

impl Default for Analysis {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_seconds: 3600,
            link_labels: std::collections::BTreeMap::new(),
            urban_boundary: None,

            observed_data: None,
            journey_survey: None,
            comparison_runs: Vec::new(),
            service: None,
            transit_observed_data: None,
            economic_inputs: None,
            emissions: None,
            noise: None,

            excess_delay_clip_seconds: None,

            zone_system: ZoneSystem::default(),
            accessibility: Accessibility::default(),
            person_group_attributes: Vec::new(),
            person_weight_attribute: None,
            person_cost_attribute: None,
        }
    }
}

impl Default for Output {
    fn default() -> Self {
        Self {
            output_dir: "./output".parse().unwrap(),
            overwrite_files: OverwriteFiles::FailIfDirectoryExists,
            profiling: Profiling::None,
            logging: Logging::Info,
            write_events: WriteEvents::File,
            analysis: Analysis::default(),
        }
    }
}

register_override!("output.output_dir", |config, value| {
    config.output_mut().output_dir = PathBuf::from(value);
});

register_override!("output.overwrite_files", |config, value| {
    config.output_mut().overwrite_files = parse_overwrite_file(value);
});

// Analysis settings are opt-in and configurable, so both are reachable from the command line.
register_override!(
    "output.analysis.enabled",
    |config, value| match value.parse() {
        Ok(enabled) => config.output_mut().analysis.enabled = enabled,
        Err(_) => warn!("Ignoring invalid analysis enabled flag '{value}': expected a boolean"),
    }
);

register_override!(
    "output.analysis.interval_seconds",
    |config, value| match value.parse() {
        Ok(interval) => config.output_mut().analysis.interval_seconds = interval,
        Err(_) => warn!("Ignoring invalid analysis interval '{value}': expected seconds"),
    }
);

register_override!(
    "output.analysis.excess_delay_clip_seconds",
    |config, value| match value.parse::<f64>() {
        Ok(limit) => config.output_mut().analysis.excess_delay_clip_seconds = Some(limit),
        Err(_) => warn!("Ignoring invalid excess delay clip '{value}': expected seconds"),
    }
);

// The accessibility inputs are file paths, so each is reachable from the command line as well
// as from YAML. `register_override!` stores a plain function pointer, so these are written out
// rather than generated from a helper that would have to capture a setter.
register_override!(
    "output.analysis.accessibility.opportunities",
    |config, value| {
        if value.is_empty() {
            warn!("Ignoring empty output.analysis.accessibility.opportunities");
            return;
        }
        config.output_mut().analysis.accessibility.opportunities = Some(PathBuf::from(value));
    }
);

register_override!("output.analysis.accessibility.zones", |config, value| {
    if value.is_empty() {
        warn!("Ignoring empty output.analysis.accessibility.zones");
        return;
    }
    config.output_mut().analysis.accessibility.zones = Some(PathBuf::from(value));
});

register_override!(
    "output.analysis.accessibility.travel_costs",
    |config, value| {
        if value.is_empty() {
            warn!("Ignoring empty output.analysis.accessibility.travel_costs");
            return;
        }
        config.output_mut().analysis.accessibility.travel_costs = Some(PathBuf::from(value));
    }
);

register_override!(
    "output.analysis.accessibility.thresholds_seconds",
    |config, value| {
        let thresholds: std::result::Result<Vec<f64>, _> = value
            .split(',')
            .map(|threshold| threshold.trim().parse::<f64>())
            .collect();
        match thresholds {
            Ok(thresholds) if !thresholds.is_empty() => {
                config
                    .output_mut()
                    .analysis
                    .accessibility
                    .thresholds_seconds = thresholds;
            }
            Ok(_) => warn!("Ignoring empty accessibility threshold list '{value}'"),
            Err(_) => warn!(
                "Ignoring invalid accessibility thresholds '{value}': expected comma-separated seconds"
            ),
        }
    }
);

// The grouping list is comma separated, so a run can add demographic dimensions without a
// config file edit. Blank entries are dropped rather than grouping everyone under an empty name.
register_override!(
    "output.analysis.person_group_attributes",
    |config, value| {
        config.output_mut().analysis.person_group_attributes = value
            .split(',')
            .map(str::trim)
            .filter(|attribute| !attribute.is_empty())
            .map(str::to_owned)
            .collect();
    }
);

register_override!(
    "output.analysis.person_weight_attribute",
    |config, value| {
        let attribute = value.trim();
        config.output_mut().analysis.person_weight_attribute =
            (!attribute.is_empty()).then(|| attribute.to_owned());
    }
);

register_override!("output.analysis.person_cost_attribute", |config, value| {
    let attribute = value.trim();
    config.output_mut().analysis.person_cost_attribute =
        (!attribute.is_empty()).then(|| attribute.to_owned());
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
    pub adaptive_reroute_probability: Option<f64>,
    pub adaptive_reroute_interval: u32,
    pub batch_previous_route_proposals: bool,
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

register_override!(
    "replanning.adaptive_reroute_probability",
    |config, value| {
        config.replanning_mut().adaptive_reroute_probability =
            (value != "none").then(|| value.parse().unwrap());
    }
);

register_override!("replanning.adaptive_reroute_interval", |config, value| {
    config.replanning_mut().adaptive_reroute_interval = value.parse().unwrap();
});

register_override!(
    "replanning.batch_previous_route_proposals",
    |config, value| {
        config.replanning_mut().batch_previous_route_proposals = value.parse().unwrap();
    }
);

impl Default for Replanning {
    fn default() -> Self {
        Self {
            fraction_of_iterations_to_disable_innovation: 1.0,
            max_agent_plan_memory: 5,
            plan_selector_for_removal: WORST_SCORE_STRATEGY_NAME.to_string(),
            adaptive_reroute_probability: None,
            adaptive_reroute_interval: 5,
            batch_previous_route_proposals: false,
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
    pub write_experienced_plans: bool,
    pub activity_params: Vec<ActivityParameter>,
    pub mode_params: Vec<ModeParameter>,
    pub agent_params: Vec<AgentParameter>,
}

register_override!("scoring.write_experienced_plans", |config, value| {
    config.scoring_mut().write_experienced_plans = value.parse().unwrap();
});

impl Scoring {
    /// Reject parameter values that the routing logic cannot turn into a finite travel-time cost.
    ///
    /// PT routing computes each ride's cost as `(performing - mode_utility) / (performing - pt_utility)`,
    /// which is undefined when `performing == pt_utility` and undefined for any non-finite input.
    /// Both of those checks belong here rather than scattered through the routing logic, because the
    /// controller cannot meaningfully start a run that produced them and because a typed validation
    /// call site is the right place to push external, user-configured inputs.
    pub fn validate(&self) -> Result<(), String> {
        for params in &self.mode_params {
            for (field, value) in [
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
            ] {
                if !value.is_finite() {
                    return Err(format!(
                        "scoring mode {} has a non-finite {field}",
                        params.mode
                    ));
                }
            }
        }
        for params in &self.agent_params {
            for (field, value) in [
                ("late_arrival", params.late_arrival),
                ("early_departure", params.early_departure),
                ("performing", params.performing),
                ("waiting", params.waiting),
                (
                    "marginal_utility_of_money",
                    params.marginal_utility_of_money,
                ),
                ("aborted_plan_score", params.aborted_plan_score),
            ] {
                if !value.is_finite() {
                    return Err(format!(
                        "scoring agent subpopulation {} has a non-finite {field}",
                        params.subpopulation
                    ));
                }
            }
        }
        for agent in &self.agent_params {
            let performing = agent.performing;
            let pt_utility = self
                .mode_params
                .iter()
                .find(|params| params.mode == "pt" && params.subpopulation == agent.subpopulation)
                .or_else(|| {
                    self.mode_params
                        .iter()
                        .find(|params| params.mode == "pt" && params.subpopulation.is_empty())
                })
                .map_or(-6.0, |params| params.marginal_utility_of_traveling);
            if performing == pt_utility {
                return Err(format!(
                    "scoring parameters for subpopulation {} must produce a non-zero travel-time cost for pt",
                    agent.subpopulation
                ));
            }
        }
        Ok(())
    }
}

impl Default for Scoring {
    fn default() -> Self {
        Self {
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
        }
    }
}

register_override!("scoring.write_experienced_plans", |config, value| {
    config.scoring_mut().write_experienced_plans = value.parse().unwrap();
});

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
#[serde(default)]
pub struct ModeParameter {
    /// Empty string means the entry applies to every subpopulation (the default); a non-empty
    /// value scopes the parameter to that subpopulation. PT routing resolves per-subpopulation
    /// costs by checking the agent's subpopulation, falling back to the empty-string entry, and
    /// then to the built-in default for the `pt` mode.
    pub subpopulation: String,
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
        Self {
            mode: mode.to_string(),
            ..Self::default()
        }
    }
}

impl Default for ModeParameter {
    fn default() -> Self {
        Self {
            subpopulation: String::new(),
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
    pub storage_capacity_factor: Option<f64>,
    pub stuck_threshold: u32,
    pub remove_stuck_vehicles: bool,
    pub main_modes: Vec<String>,
    /// Paths to the MATSim signal files. Absent, or present but incomplete, means the
    /// run has no signals.
    pub signals: SignalFilesConfig,
}

/// The three MATSim signal files that together define a signal plan.
///
/// All three are required: a partial set is a configuration error rather than a silent
/// run with no signals, so `validate` reports it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct SignalFilesConfig {
    pub systems: Option<String>,
    pub groups: Option<String>,
    pub control: Option<String>,
}

impl SignalFilesConfig {
    pub fn any_present(&self) -> bool {
        self.systems.is_some() || self.groups.is_some() || self.control.is_some()
    }

    pub fn validate(&self) -> Result<(), String> {
        if !self.any_present() {
            return Ok(());
        }
        if self.systems.is_some() && self.groups.is_some() && self.control.is_some() {
            return Ok(());
        }
        Err(
            "qsim.signals needs all three of systems, groups and control; \
             a partial set cannot define a signal plan"
                .to_string(),
        )
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct TravelTimeCalculator {
    /// Width of a travel-time interval in seconds.
    pub bin_size: u32,
}

impl TravelTimeCalculator {
    pub fn validate(&self) -> Result<(), String> {
        if self.bin_size == 0 {
            return Err("travel_time_calculator.bin_size must be greater than 0".to_string());
        }
        Ok(())
    }
}

impl Default for TravelTimeCalculator {
    fn default() -> Self {
        Self { bin_size: 900 }
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
        is_last_iteration || iteration.is_multiple_of(self.write_plans_interval)
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
            storage_capacity_factor: None,
            stuck_threshold: value.stuck_threshold,
            remove_stuck_vehicles: false,
            main_modes: value.main_modes.clone(),
            signals: SignalFilesConfig::default(),
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

register_override!("qsim.remove_stuck_vehicles", |config, value| {
    config.qsim_mut().remove_stuck_vehicles = value.parse().unwrap();
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
            storage_capacity_factor: None,
            stuck_threshold: 10,
            remove_stuck_vehicles: false,
            main_modes: vec![],
            signals: SignalFilesConfig::default(),
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

    /// Inverse of [`CompressionType::extension`]; accepts the full recorded format string.
    pub fn from_extension(extension: &str) -> Option<Self> {
        match extension {
            "xml" => Some(Self::None),
            "xml.gz" => Some(Self::Gz),
            "binpb" => Some(Self::Proto),
            "xml.zst" => Some(Self::Zst),
            _ => None,
        }
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
    #[serde(default = "f32_value_0_03")]
    pub imbalance_factor: f32,
    #[serde(default = "u32_value_100")]
    pub iteration_number: u32,
    #[serde(default = "bool_value_false")]
    pub contiguous: bool,
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
            imbalance_factor: 0.03,
            iteration_number: 10,
            contiguous: true,
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

    pub fn ufactor(&self) -> usize {
        let val = (self.imbalance_factor * 1000.) as usize;
        if val == 0 {
            return 1;
        };
        val
    }

    pub fn set_contiguous(mut self, contiguous: bool) -> Self {
        self.contiguous = contiguous;
        self
    }
}

fn f32_value_0_03() -> f32 {
    0.03
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

fn default_vertex_weight() -> Vec<VertexWeight> {
    vec![InLinkCapacity]
}

fn default_profiling_level() -> String {
    String::from("INFO")
}

#[cfg(test)]
mod tests {
    use crate::simulation::config;
    use crate::simulation::config::Analysis;
    use crate::simulation::config::Output;
    use crate::simulation::config::OverwriteFiles;
    use crate::simulation::config::PathBuf;
    use crate::simulation::config::Profiling;
    use crate::simulation::config::TransferConstruction;
    use crate::simulation::config::WriteEvents;
    use crate::simulation::config::{
        ActivityParameter, AgentParameter, CommandLineArgs, CompressionType, ComputationalSetup,
        Config, Controller, EdgeWeight, MetisOptions, ModeParameter, PartitionMethod, Partitioning,
        QSim, Replanning, Routing, Scoring, SignalFilesConfig, StrategySetting, TeleportedParams,
        TravelTimeCalculator, VertexWeight, parse_key_val,
    };
    use crate::simulation::config::{Ids, Network, Population, Transit, Vehicles};
    use crate::simulation::config::{
        IntermodalAccessEgress, IntermodalLegOnlyHandling, IntermodalModeSelection, Logging,
        ModalLinkSelection, RoutingMode,
    };
    use crate::simulation::replanning::{
        KEEP_LAST_SELECTED_STRATEGY_NAME, WORST_SCORE_STRATEGY_NAME,
    };
    use std::io::Write;
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
            }),
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
            storage_capacity_factor: None,
            stuck_threshold: 1,
            remove_stuck_vehicles: true,
            main_modes: vec!["bike".to_string()],
            signals: SignalFilesConfig::default(),
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
            })
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
        assert!(parsed_config.qsim().remove_stuck_vehicles);
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
    fn compression_type_extension_round_trips() {
        for compression in [
            CompressionType::None,
            CompressionType::Gz,
            CompressionType::Proto,
            CompressionType::Zst,
        ] {
            assert_eq!(
                CompressionType::from_extension(compression.extension()),
                Some(compression),
                "{}",
                compression.extension()
            );
        }
        assert_eq!(CompressionType::from_extension("parquet"), None);
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
        assert!(!parsed_config.qsim().remove_stuck_vehicles);
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
    fn transit_transfer_construction_is_configurable_and_validated() {
        for (value, expected) in [
            ("initial", TransferConstruction::Initial),
            ("adaptive", TransferConstruction::Adaptive),
            ("online", TransferConstruction::Online),
        ] {
            let yaml = format!(
                "modules:\n  transit:\n    type: Transit\n    transfer_construction: {value}\n"
            );
            let parsed: Config = serde_yaml::from_str(&yaml).unwrap();
            assert_eq!(parsed.transit().transfer_construction, expected);
        }
        let invalid =
            "modules:\n  transit:\n    type: Transit\n    transfer_construction: unknown\n";
        assert!(serde_yaml::from_str::<Config>(invalid).is_err());
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
                adaptive_reroute_probability: None,
                adaptive_reroute_interval: 5,
                batch_previous_route_proposals: false,
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
                adaptive_reroute_probability: None,
                adaptive_reroute_interval: 5,
                batch_previous_route_proposals: false,
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
        "#;

        let config: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
        let expected = Scoring {
            write_experienced_plans: true,
            activity_params: vec![ActivityParameter {
                activity_type: "home".to_string(),
                typical_duration_s: 43_200.0,
            }],
            mode_params: vec![ModeParameter {
                subpopulation: String::new(),
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
        assert_eq!(
            MetisOptions::default().set_imbalance_factor(0.03).ufactor(),
            30
        );
        assert_eq!(
            MetisOptions::default()
                .set_imbalance_factor(0.001)
                .ufactor(),
            1
        );
        assert_eq!(
            MetisOptions::default()
                .set_imbalance_factor(0.00001)
                .ufactor(),
            1
        );
        assert_eq!(
            MetisOptions::default()
                .set_imbalance_factor(0.00000)
                .ufactor(),
            1
        );
        assert_eq!(
            MetisOptions::default().set_imbalance_factor(-1.).ufactor(),
            1
        );
        assert_eq!(
            MetisOptions::default().set_imbalance_factor(1.1).ufactor(),
            1100
        );
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
        assert!(!config.transit().simulate_vehicles);
        assert_eq!(vec!["pt"], config.transit().transit_modes);
        assert!(!config.transit().use_mode_mapping_for_passengers);
        assert!(config.transit().mode_mapping_for_passengers.is_empty());
        assert!(!config.transit().personless_car_fallback);
        assert!(config.transit().range_query_settings.is_empty());
        assert!(config.transit().route_selector_settings.is_empty());
    }

    #[test]
    fn transit_range_query_and_selector_settings_load_from_yaml() {
        let file = write_temp_config(
            r#"
modules:
  transit:
    type: Transit
    range_query_settings:
      - max_earlier_departure_sec: 300
        max_later_departure_sec: 600
        subpopulations: [freight]
    route_selector_settings:
      - beta_travel_time: 1.0
        beta_departure_time: 0.5
        beta_transfer_count: 300.0
        subpopulations: [freight]
"#,
        );
        let config = Config::from_args(CommandLineArgs {
            config: file.path().to_str().unwrap().to_string(),
            overrides: vec![],
        });

        assert_eq!(
            300,
            config.transit().range_query_settings[0].max_earlier_departure_sec
        );
        assert_eq!(
            600,
            config.transit().range_query_settings[0].max_later_departure_sec
        );
        assert_eq!(
            "freight",
            config.transit().route_selector_settings[0].subpopulations[0]
        );
    }

    /// The legacy fallback for queries without a person is off unless a config asks for it, and
    /// it can be asked for without touching passenger behaviour.
    #[test]
    fn personless_car_fallback_is_opt_in() {
        let file = write_temp_config(
            r#"
modules:
  transit:
    type: Transit
    schedule_path: schedule.xml
"#,
        );
        let config = Config::from_args(CommandLineArgs {
            config: file.path().to_str().unwrap().to_string(),
            overrides: vec![(
                "transit.personless_car_fallback".to_string(),
                "true".to_string(),
            )],
        });

        assert!(config.transit().personless_car_fallback);
    }

    #[test]
    fn transit_vehicle_simulation_reads_from_yaml_and_overrides() {
        let yaml = r#"
modules:
  transit:
    type: Transit
    schedule_path: schedule.xml
"#;
        let parsed: Config = serde_yaml::from_str(yaml).expect("failed to parse config");
        assert!(!parsed.transit().simulate_vehicles);
        assert_eq!(vec!["pt"], parsed.transit().transit_modes);

        let file = write_temp_config(yaml);
        let config = Config::from_args(CommandLineArgs {
            config: file.path().to_str().unwrap().to_string(),
            overrides: vec![
                ("transit.simulate_vehicles".to_string(), "true".to_string()),
                ("transit.transit_modes".to_string(), "bus, rail".to_string()),
            ],
        });
        assert!(config.transit().simulate_vehicles);
        assert_eq!(vec!["bus", "rail"], config.transit().transit_modes);
    }

    #[test]
    fn intermodal_transit_settings_read_from_yaml() {
        let yaml = r#"
modules:
  transit:
    type: Transit
    use_intermodal_access_egress: true
    intermodal_access_egress_mode_selection: random_per_direction
    intermodal_leg_only_handling: avoid
    intermodal_access_egress:
      - mode: bike
        initial_search_radius: 750.0
        max_radius: 4000.0
        search_extension_radius: 500.0
        share_trip_search_radius: 0.5
        person_filter_attribute: ownsBike
        person_filter_value: true
        stop_filter_attribute: bikeAccess
        stop_filter_value: true
        link_id_attribute: bikeLink
"#;
        let parsed: Config = serde_yaml::from_str(yaml).expect("valid intermodal transit config");
        assert!(parsed.transit().use_intermodal_access_egress);
        assert_eq!(
            IntermodalModeSelection::RandomPerDirection,
            parsed.transit().intermodal_access_egress_mode_selection
        );
        assert_eq!(
            IntermodalLegOnlyHandling::Avoid,
            parsed.transit().intermodal_leg_only_handling
        );
        assert_eq!(
            vec![IntermodalAccessEgress {
                mode: "bike".to_owned(),
                initial_search_radius: 750.0,
                max_radius: 4000.0,
                search_extension_radius: 500.0,
                share_trip_search_radius: 0.5,
                person_filter_attribute: Some("ownsBike".to_owned()),
                person_filter_value: Some("true".to_owned()),
                stop_filter_attribute: Some("bikeAccess".to_owned()),
                stop_filter_value: Some("true".to_owned()),
                link_id_attribute: Some("bikeLink".to_owned()),
            }],
            parsed.transit().intermodal_access_egress
        );
    }

    #[test]
    fn transit_passenger_mode_mappings_read_from_yaml() {
        let config: Config = serde_yaml::from_str(
            r#"
modules:
  transit:
    type: Transit
    use_mode_mapping_for_passengers: true
    transit_modes: [rail, road]
    mode_mapping_for_passengers:
      train: rail
      bus: road
"#,
        )
        .expect("failed to parse passenger mode mappings");

        assert!(config.transit().use_mode_mapping_for_passengers);
        assert_eq!(
            "rail",
            config.transit().mode_mapping_for_passengers["train"]
        );
        assert_eq!("road", config.transit().mode_mapping_for_passengers["bus"]);
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
            analysis: Analysis::default(),
        });
        config.set_partitioning(Partitioning {
            num_parts: 1,
            method: PartitionMethod::None,
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
        assert_eq!(300, config.travel_time_calculator().bin_size);

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
            &TravelTimeCalculator { bin_size: 60 },
            config.travel_time_calculator()
        );
    }

    #[test]
    fn travel_time_calculator_rejects_zero_bin_size() {
        let calculator = TravelTimeCalculator { bin_size: 0 };

        assert_eq!(
            Err("travel_time_calculator.bin_size must be greater than 0".to_string()),
            calculator.validate()
        );
    }

    #[test]
    fn scoring_validate_rejects_non_finite_mode_params() {
        let mut scoring = Scoring::default();
        scoring.mode_params[0].marginal_utility_of_traveling = f64::NAN;

        let error = scoring.validate().unwrap_err();
        assert!(
            error.contains("scoring mode car has a non-finite marginal_utility_of_traveling"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn scoring_validate_rejects_non_finite_agent_params() {
        let mut scoring = Scoring::default();
        scoring.agent_params[0].performing = f64::INFINITY;

        let error = scoring.validate().unwrap_err();
        assert!(
            error.contains("scoring agent subpopulation person has a non-finite performing"),
            "unexpected error: {error}"
        );
    }

    /// `Scoring::validate()` must catch the `performing == pt_utility` divide-by-zero for *every*
    /// agent subpopulation, not only the hard-coded "person" one. The pre-issue-75 implementation
    /// only checked "person", so a freight subpopulation with a matching utility would slip through
    /// and break transit routing at runtime.
    #[test]
    fn scoring_validate_catches_performing_eq_pt_utility_for_every_subpopulation() {
        let mut scoring = Scoring::default();
        // Add a second agent_params entry whose performing matches the empty-subpopulation
        // `pt` utility (-6.0) and would have escaped the old "person"-only check.
        scoring.agent_params.push({
            let mut params = scoring.agent_params[0].clone();
            params.subpopulation = "freight".to_string();
            params.performing = -6.0;
            params
        });

        let error = scoring.validate().unwrap_err();
        assert!(
            error.contains("subpopulation freight"),
            "unexpected error: {error}"
        );
    }
}
