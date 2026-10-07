pub mod facilities;
pub mod network;
pub mod population;
pub mod prepare;
pub mod transit;
pub mod trip_structure_utils;
pub mod vehicles;

use crate::simulation::config::Config;
use crate::simulation::network::LinkStorageCapacities;
use crate::simulation::network::sim_network::SimNetworkPartition;
use crate::simulation::{id, io};
use facilities::ActivityFacilities;
use network::Network;
use population::Population;
use std::sync::Arc;
use tracing::info;
use transit::TransitSchedule;
use vehicles::Garage;

#[derive(Debug, Clone, PartialEq)]
pub struct Coordinate {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

impl Coordinate {
    pub fn new_2d(x: f64, y: f64) -> Self {
        Self { x, y, z: 0. }
    }

    pub fn new_3d(x: f64, y: f64, z: f64) -> Self {
        Self { x, y, z }
    }

    pub fn euclidean_distance(a: &Coordinate, b: &Coordinate) -> f64 {
        let dx = a.x - b.x;
        let dy = a.y - b.y;
        let dz = a.z - b.z;
        (dx * dx + dy * dy + dz * dz).sqrt()
    }

    pub fn middle(a: &Self, b: &Self) -> Self {
        Coordinate::new_3d((a.x + b.x) / 2., (a.y + b.y) / 2., (a.z + b.z) / 2.)
    }

    /// Returns the orthogonal projection of `point` onto the line segment
    /// defined by `line_from` and `line_to`.
    ///
    /// The returned coordinate is the closest point on that segment to `point`.
    pub fn orthogonal_projection(point: &Self, line_from: &Self, line_to: &Self) -> Self {
        // Orthogonal projection of point onto the segment from and to:
        // v = from - to
        // t = dot(point - from, v) / dot(v, v)
        // projection = from + t * v

        let dx = line_to.x - line_from.x;
        let dy = line_to.y - line_from.y;
        let dz = line_to.z - line_from.z;
        let segment_length_squared = dx * dx + dy * dy + dz * dz;

        // line has 0 length
        if segment_length_squared == 0.0 {
            return line_from.clone();
        }

        let t = (((point.x - line_from.x) * dx
            + (point.y - line_from.y) * dy
            + (point.z - line_from.z) * dz)
            / segment_length_squared)
            .clamp(0.0, 1.0);

        Coordinate::new_3d(
            line_from.x + t * dx,
            line_from.y + t * dy,
            line_from.z + t * dz,
        )
    }
}

impl Default for Coordinate {
    fn default() -> Self {
        Self::new_3d(0.0, 0.0, 0.0)
    }
}

#[cfg(test)]
mod coordinate_tests {
    use super::Coordinate;
    use assert_approx_eq::assert_approx_eq;

    fn assert_coordinate_eq(expected: Coordinate, actual: Coordinate) {
        assert_approx_eq!(expected.x, actual.x);
        assert_approx_eq!(expected.y, actual.y);
        assert_approx_eq!(expected.z, actual.z);
    }

    #[test]
    fn orthogonal_projection_inside_segment_keeps_projection() {
        let projection = Coordinate::orthogonal_projection(
            &Coordinate::new_2d(5.0, 4.0),
            &Coordinate::new_2d(0.0, 0.0),
            &Coordinate::new_2d(10.0, 0.0),
        );

        assert_coordinate_eq(Coordinate::new_2d(5.0, 0.0), projection);
    }

    #[test]
    fn orthogonal_projection_before_segment_clamps_to_from() {
        let projection = Coordinate::orthogonal_projection(
            &Coordinate::new_2d(-5.0, 4.0),
            &Coordinate::new_2d(0.0, 0.0),
            &Coordinate::new_2d(10.0, 0.0),
        );

        assert_coordinate_eq(Coordinate::new_2d(0.0, 0.0), projection);
    }

    #[test]
    fn orthogonal_projection_after_segment_clamps_to_to() {
        let projection = Coordinate::orthogonal_projection(
            &Coordinate::new_2d(15.0, 4.0),
            &Coordinate::new_2d(0.0, 0.0),
            &Coordinate::new_2d(10.0, 0.0),
        );

        assert_coordinate_eq(Coordinate::new_2d(10.0, 0.0), projection);
    }

    #[test]
    fn orthogonal_projection_clamps_on_3d_segment() {
        let projection = Coordinate::orthogonal_projection(
            &Coordinate::new_3d(7.0, 7.0, 7.0),
            &Coordinate::new_3d(0.0, 0.0, 0.0),
            &Coordinate::new_3d(2.0, 2.0, 2.0),
        );

        assert_coordinate_eq(Coordinate::new_3d(2.0, 2.0, 2.0), projection);
    }

    #[test]
    fn orthogonal_projection_zero_length_segment_returns_endpoint() {
        let endpoint = Coordinate::new_3d(1.0, 2.0, 3.0);
        let projection = Coordinate::orthogonal_projection(
            &Coordinate::new_3d(7.0, 7.0, 7.0),
            &endpoint,
            &endpoint,
        );

        assert_coordinate_eq(endpoint, projection);
    }
}

/// The scenario as it comes from input files: fully owned and still local to the loading thread.
#[derive(Debug)]
pub struct Scenario {
    pub network: Network,
    pub garage: Garage,
    pub population: Population,
    pub transit_schedule: TransitSchedule,
    pub facilities: ActivityFacilities,
    pub config: Arc<Config>,
}

impl Scenario {
    pub fn load<C: Into<Arc<Config>>>(config: C) -> Self {
        info!("Start loading mod.");

        let config = config.into();

        if let Some(path) = &config.ids().path {
            info!("Loading IDs from {:?}", path);
            id::load_from_file(&io::resolve_path(config.context(), path));
        }

        // mandatory content to create a mod
        let network = Self::load_network(&config);
        let mut garage = Self::load_garage(&config);
        let transit_schedule = Self::load_transit_schedule(&config);
        // Facilities are loaded before the population, so that their ids exist when activities
        // reference them.
        let facilities = Self::load_facilities(&config);
        let population = Self::load_population(&config, &mut garage);

        Scenario {
            network,
            garage,
            population,
            transit_schedule,
            facilities,
            config,
        }
    }

    fn load_network(config: &Config) -> Network {
        if let Some(path) = &config.network().path {
            let net_in_path = io::resolve_path(config.context(), path);
            let num_parts = config.partitioning().num_parts;
            Network::from_file_path(&net_in_path, num_parts, &config.partitioning().method)
        } else {
            Network::default()
        }
    }

    fn load_garage(config: &Config) -> Garage {
        if let Some(path) = &config.vehicles().path {
            let garage_in_path = io::resolve_path(config.context(), path);
            Garage::from_file(&garage_in_path)
        } else {
            Garage::default()
        }
    }

    fn load_population(config: &Config, garage: &mut Garage) -> Population {
        if let Some(path) = &config.population().path {
            let pop_in_path = io::resolve_path(config.context(), path);
            Population::from_file(&pop_in_path, garage)
        } else {
            Population::default()
        }
    }

    fn load_facilities(config: &Config) -> ActivityFacilities {
        if let Some(path) = &config.facilities().path {
            let facilities_in_path = io::resolve_path(config.context(), path);
            ActivityFacilities::from_file(&facilities_in_path)
        } else {
            ActivityFacilities::default()
        }
    }

    fn load_transit_schedule(config: &Config) -> TransitSchedule {
        if let Some(path) = &config.transit().schedule_path {
            let schedule_in_path = io::resolve_path(config.context(), path);
            TransitSchedule::from_file(&schedule_in_path)
        } else {
            TransitSchedule::default()
        }
    }
}

/// Immutable scenario data shared by controller, mobsim partitions and replanning phases.
#[derive(Debug, Clone, Default)]
pub struct ScenarioCore {
    pub network: Arc<Network>,
    pub garage: Arc<Garage>,
    pub transit_schedule: Arc<TransitSchedule>,
    /// Activity facilities, prepared by `prepare_for_sim` before the controller shares them.
    pub facilities: Arc<ActivityFacilities>,
    pub config: Arc<Config>,
}

/// Controller-owned scenario state between phases.
#[derive(Debug)]
pub struct ControllerScenario {
    pub core: ScenarioCore,
    pub population: Population,
}

/// Owned population fragment passed between execution phases.
#[derive(Debug, Default)]
pub struct PopulationShard {
    pub population: Population,
}

/// Static and per-run runtime context for one mobsim partition.
#[derive(Debug)]
pub struct MobsimScenarioPartition {
    pub rank: u32,
    pub scenario: ScenarioCore,
    pub network_partition: SimNetworkPartition,
}

/// Input for one mobsim partition run.
#[derive(Debug)]
pub struct MobsimInput {
    pub partition: MobsimScenarioPartition,
    pub population: PopulationShard,
}

impl From<Scenario> for ControllerScenario {
    fn from(scenario: Scenario) -> Self {
        Self {
            core: ScenarioCore {
                network: Arc::new(scenario.network),
                garage: Arc::new(scenario.garage),
                transit_schedule: Arc::new(scenario.transit_schedule),
                facilities: Arc::new(scenario.facilities),
                config: scenario.config,
            },
            population: scenario.population,
        }
    }
}

impl ControllerScenario {
    pub(crate) fn split_for_mobsim(
        &mut self,
        storage_capacities: &LinkStorageCapacities,
    ) -> Vec<MobsimInput> {
        let num_parts = self.core.config.partitioning().num_parts;
        let population = std::mem::take(&mut self.population);
        population
            .split_by_start_link_partition(&self.core.network, num_parts)
            .into_iter()
            .enumerate()
            .map(|(rank, population)| {
                self.create_mobsim_input(rank as u32, population, storage_capacities)
            })
            .collect()
    }

    #[cfg(test)]
    pub fn merge_population_shards(&mut self, shards: Vec<PopulationShard>) {
        for shard in shards {
            for (id, person) in shard.population.persons {
                let previous = self.population.persons.insert(id.clone(), person);
                assert!(
                    previous.is_none(),
                    "Person {id} was returned by more than one population shard"
                );
            }
        }
    }

    pub fn replace_population(&mut self, population: Population) {
        assert!(
            self.population.persons.is_empty(),
            "Controller still owns population while replacing it after a phase."
        );
        self.population = population;
    }

    fn create_mobsim_input(
        &self,
        rank: u32,
        population: Population,
        storage_capacities: &LinkStorageCapacities,
    ) -> MobsimInput {
        let network_partition =
            Self::create_network_partition(&self.core, storage_capacities, rank);

        info!(
            "Partition #{rank} network has: {} nodes and {} links. Population has {} agents",
            network_partition.nodes.len(),
            network_partition.links.len(),
            population.persons.len()
        );

        MobsimInput {
            partition: MobsimScenarioPartition {
                rank,
                // Since core holds Arcs, this clone is cheap.
                scenario: self.core.clone(),
                network_partition,
            },
            population: PopulationShard { population },
        }
    }

    fn create_network_partition(
        core: &ScenarioCore,
        storage_capacities: &LinkStorageCapacities,
        rank: u32,
    ) -> SimNetworkPartition {
        SimNetworkPartition::from_network(&core.network, storage_capacities, rank, &core.config)
    }
}

#[cfg(test)]
mod tests {
    use super::{ControllerScenario, Scenario};
    use crate::simulation::config::{Config, PartitionMethod, Transit};
    use crate::simulation::id::Id;
    use crate::simulation::network::LinkStorageCapacities;
    use crate::simulation::scenario::facilities::{ActivityFacilities, ActivityFacility};
    use crate::simulation::scenario::network::{Link, Network};
    use crate::simulation::scenario::population::Population;
    use crate::simulation::scenario::prepare::prepare_for_sim::prepare_for_sim;
    use crate::simulation::scenario::transit::{
        TransitLine, TransitRoute, TransitSchedule, TransitStopFacility,
    };
    use crate::simulation::scenario::vehicles::Garage;
    use macros::deterministic_id_test;
    use std::path::PathBuf;
    use std::sync::Arc;

    #[deterministic_id_test]
    fn scenario_without_transit_config_uses_shared_empty_schedule() {
        let scenario = Scenario::load(Config::default());

        assert!(scenario.transit_schedule.lines().is_empty());
        assert!(scenario.transit_schedule.facilities().is_empty());

        let controller_scenario: ControllerScenario = scenario.into();
        assert!(controller_scenario.core.transit_schedule.lines().is_empty());
        assert!(
            controller_scenario
                .core
                .transit_schedule
                .facilities()
                .is_empty()
        );
    }

    #[deterministic_id_test]
    fn scenario_loads_xml_transit_schedule_and_creates_ids() {
        let mut config = Config::default();
        config.set_transit(Transit {
            schedule_path: Some("./assets/pt_tutorial/transitschedule.xml".into()),
        });

        let scenario = Scenario::load(config);

        assert_eq!(1, scenario.transit_schedule.lines().len());
        assert_eq!(4, scenario.transit_schedule.facilities().len());
        assert_eq!(2, scenario.transit_schedule.num_routes());
        assert_eq!(
            "Blue Line",
            Id::<TransitLine>::get_from_ext("Blue Line").external()
        );
        assert_eq!("1to3", Id::<TransitRoute>::get_from_ext("1to3").external());
        assert_eq!("1", Id::<TransitStopFacility>::get_from_ext("1").external());
        assert_eq!("1to3", Id::<String>::get_from_ext("1to3").external());
    }

    #[deterministic_id_test]
    fn scenario_loads_facilities_and_prepare_for_sim_prepares_them() {
        let mut config = Config::default();
        config.network_mut().path = Some("./assets/equil/equil-network.xml".into());
        config.facilities_mut().path =
            Some("./tests/resources/facilities/equil-facilities.xml".into());

        let mut scenario = Scenario::load(config);

        assert_eq!(2, scenario.facilities.facilities.len());
        let home_id = Id::<ActivityFacility>::get_from_ext("home");
        assert_eq!(None, scenario.facilities.get(&home_id).unwrap().base_link);

        prepare_for_sim(&mut scenario).unwrap();

        let facilities = &scenario.facilities;
        let home = facilities.get(&home_id).unwrap();
        let work = facilities
            .get(&Id::<ActivityFacility>::get_from_ext("work"))
            .unwrap();
        assert_eq!(Some(Id::<Link>::get_from_ext("1")), home.base_link);
        assert_eq!(Some(Id::<Link>::get_from_ext("20")), work.base_link);
        assert_eq!(1, work.activities[0].open_times.len());
    }

    #[deterministic_id_test]
    fn split_and_merge_mobsim_population_keeps_every_person_once() {
        let mut garage = Garage::from_file(&PathBuf::from("./assets/3-links/vehicles.xml"));
        let population = Population::from_file("./assets/3-links/3-agent.xml", &mut garage);
        let original_len = population.persons.len();
        let config = Arc::new(Config::default());
        let network = Network::from_file(
            "./assets/3-links/3-links-network.xml",
            config.partitioning().num_parts,
            &PartitionMethod::None,
        );

        let storage_capacities = LinkStorageCapacities::from_network(&network, config.qsim());
        let mut scenario: ControllerScenario = Scenario {
            network,
            garage,
            population,
            transit_schedule: TransitSchedule::default(),
            facilities: ActivityFacilities::default(),
            config,
        }
        .into();

        let inputs = scenario.split_for_mobsim(&storage_capacities);

        assert!(scenario.population.persons.is_empty());

        let shards = inputs.into_iter().map(|input| input.population).collect();
        scenario.merge_population_shards(shards);

        assert_eq!(original_len, scenario.population.persons.len());
    }
}
