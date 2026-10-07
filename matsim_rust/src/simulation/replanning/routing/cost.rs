use crate::simulation::config::Config;
use crate::simulation::id::Id;
use crate::simulation::replanning::routing::travel_time_calculator::{
    GlobalTravelTimeCalculator, TravelTimeGetter,
};
use crate::simulation::scenario::network::Link;
use crate::simulation::scenario::population::InternalPerson;
use crate::simulation::scenario::vehicles::InternalVehicle;
use crate::simulation::time::SimTime;
use nohash_hasher::IntMap;
use std::fmt::Debug;
use std::num::FpCategory;
use std::sync::Arc;
use std::time::Duration;

/// Disutility is the unit of the cost values used in routing
pub type Disutility = f64;

/// Travel time function, mapping any network link to a travel time, depending on the departure time
/// and optionally the person and vehicle.
pub trait TravelTime: Debug + Send + Sync {
    /// get travel time of a given link at a given time, optionally for a specific person and vehicle
    fn travel_time(
        &self,
        link: &Link,
        departure_time: SimTime,
        person: Option<&InternalPerson>,
        vehicle: Option<&InternalVehicle>,
    ) -> Duration;
}

/// Travel disutility function, mapping any network link to a travel disutility, depending on the
/// departure time and optionally the person and vehicle.
/// Also provides a method to get a global lower bound on the travel disutility for a given link,
/// over all times, persons and vehicles. This is used to calculate landmark data.
///
/// # Contract
/// - Returned disutilities are used as edge weights in Dijkstra/A* and therefore must be
///   non-negative. NaN is treated as worse than infinity.
/// - `get_link_min_travel_disutility(link)` must be a global lower bound on `travel_disutility`
///   for the given link across all times/persons/vehicles (so ALT remains admissible).
pub trait TravelDisutility: Debug + Send + Sync {
    /// get travel disutility of the given link at the given time, optionally for a specific person
    /// and vehicle
    fn travel_disutility(
        &self,
        link: &Link,
        departure_time: SimTime,
        person: Option<&InternalPerson>,
        vehicle: Option<&InternalVehicle>,
    ) -> Disutility;

    /// Returns the smallest possible travel disutility at the given link, over all times, persons
    /// and vehicles.
    /// This is used when calculating landmark data, to ensure that the ALT heuristic never
    /// overestimates the travel disutility between two nodes.
    fn get_link_min_travel_disutility(&self, link: &Link) -> Disutility;
}

#[derive(Clone, Debug)]
struct SubpopulationParams {
    performing: f64,
    marginal_utility_of_money: f64,
}

#[derive(Clone, Debug)]
pub struct ScoringBasedTravelTimeAndDisutility {
    mode: Id<String>,
    params_per_subpopulation: IntMap<Id<String>, SubpopulationParams>,
    min_params: SubpopulationParams,
    monetary_distance_rate: f64,
    marginal_utility_traveling: f64,
    marginal_utility_distance: f64,
    travel_time: Arc<GlobalTravelTimeCalculator>,
}

impl ScoringBasedTravelTimeAndDisutility {
    pub fn new(
        config: &Config,
        mode: Id<String>,
        travel_time: Arc<GlobalTravelTimeCalculator>,
    ) -> Self {
        let params_per_subpopulation = config
            .scoring()
            .agent_params
            .iter()
            .map(|p| {
                (
                    Id::create(&p.subpopulation),
                    SubpopulationParams {
                        performing: p.performing,
                        marginal_utility_of_money: p.marginal_utility_of_money,
                    },
                )
            })
            .collect::<IntMap<_, _>>();

        let min_performing = params_per_subpopulation
            .iter()
            .map(|(_, params)| params.performing)
            .reduce(f64::min)
            .unwrap();

        let min_marginal_utility_of_money = params_per_subpopulation
            .iter()
            .map(|(_, params)| params.marginal_utility_of_money)
            .reduce(f64::min)
            .unwrap();

        let mode_params = config
            .scoring()
            .mode_params
            .iter()
            .find(|params| params.mode == mode.external())
            .unwrap_or_else(|| {
                panic!(
                    "No scoring parameters configured for network mode {}",
                    mode.external()
                )
            });

        ScoringBasedTravelTimeAndDisutility {
            mode,
            params_per_subpopulation,
            min_params: SubpopulationParams {
                performing: min_performing,
                marginal_utility_of_money: min_marginal_utility_of_money,
            },
            monetary_distance_rate: mode_params.monetary_distance_cost_rate,
            marginal_utility_traveling: mode_params.marginal_utility_of_traveling,
            marginal_utility_distance: mode_params.marginal_utility_of_distance,
            travel_time,
        }
    }

    fn disutility(
        &self,
        link: &Link,
        travel_time: Duration,
        person: Option<&InternalPerson>,
    ) -> Disutility {
        let person_params = if let Some(person) = person {
            self.params_per_subpopulation
                .get(person.subpopulation())
                .unwrap_or_else(|| {
                    panic!(
                        "No scoring parameters configured for subpopulation {}",
                        person.subpopulation().external()
                    )
                })
        } else {
            // Person-less routing uses the same conservative coefficient as the global lower bound.
            &self.min_params
        };

        // traveling is normally negative, so convert it into positive disutility by negating it
        let travel_cost_factor =
            (-self.marginal_utility_traveling + person_params.performing) / 3600.;

        let distance_term = (-self.marginal_utility_distance
            - self.monetary_distance_rate * person_params.marginal_utility_of_money)
            * link.length;

        travel_time.as_secs_f64() * travel_cost_factor + distance_term
    }
}

impl TravelTime for ScoringBasedTravelTimeAndDisutility {
    fn travel_time(
        &self,
        link: &Link,
        departure_time: SimTime,
        _person: Option<&InternalPerson>,
        vehicle: Option<&InternalVehicle>,
    ) -> Duration {
        self.travel_time.get_link_travel_time(
            &self.mode,
            link,
            departure_time,
            vehicle,
            TravelTimeGetter::Average,
        )
    }
}

impl TravelDisutility for ScoringBasedTravelTimeAndDisutility {
    fn travel_disutility(
        &self,
        link: &Link,
        departure_time: SimTime,
        person: Option<&InternalPerson>,
        vehicle: Option<&InternalVehicle>,
    ) -> Disutility {
        self.disutility(
            link,
            self.travel_time(link, departure_time, person, vehicle),
            person,
        )
    }

    fn get_link_min_travel_disutility(&self, link: &Link) -> Disutility {
        self.disutility(link, travel_time(link.length, link.freespeed), None)
    }
}

/// An implementation of both `TravelTime` and `TravelDisutility`, purely based on freespeed travel
/// times. The travel time is simply the link length divided by the freespeed, ignoring any given
/// vehicle type and its max speed.
///
/// The travel disutility is equal to the travel time.
#[derive(Clone, Debug)]
pub struct FreeSpeedTravelTimeAndDisutility {}

impl TravelTime for FreeSpeedTravelTimeAndDisutility {
    fn travel_time(
        &self,
        link: &Link,
        _departure_time: SimTime,
        _person: Option<&InternalPerson>,
        _vehicle: Option<&InternalVehicle>,
    ) -> Duration {
        // the given vehicle type is ignored => true freespeed
        travel_time(link.length, link.freespeed)
    }
}

impl TravelDisutility for FreeSpeedTravelTimeAndDisutility {
    fn travel_disutility(
        &self,
        link: &Link,
        departure_time: SimTime,
        person: Option<&InternalPerson>,
        vehicle: Option<&InternalVehicle>,
    ) -> Disutility {
        self.travel_time(link, departure_time, person, vehicle)
            .as_secs_f64()
            * 1.0
    }

    // min travel disutility is equal to the travel disutility, since it does not depend on time, person or vehicle
    fn get_link_min_travel_disutility(&self, link: &Link) -> Disutility {
        self.travel_disutility(link, SimTime::from_secs(0), None, None)
    }
}

/// An implementation of both `TravelTime` and `TravelDisutility`, mostly based on freespeed travel
/// times. However, when a vehicle is given, its max speed is respected, with min(freespeed, v_max)
/// being used to determine the travel time.
/// The travel disutility is equal to the travel time.
#[derive(Clone, Debug)]
pub struct FreeOrMaxSpeedTravelTimeAndDisutility;

impl TravelTime for FreeOrMaxSpeedTravelTimeAndDisutility {
    fn travel_time(
        &self,
        link: &Link,
        _departure_time: SimTime,
        _person: Option<&InternalPerson>,
        vehicle: Option<&InternalVehicle>,
    ) -> Duration {
        // respect the given vehicle type, if provided
        let max_speed = if let Some(v) = vehicle {
            v.max_v.min(link.freespeed)
        } else {
            link.freespeed
        };

        travel_time(link.length, max_speed)
    }
}

impl TravelDisutility for FreeOrMaxSpeedTravelTimeAndDisutility {
    fn travel_disutility(
        &self,
        link: &Link,
        departure_time: SimTime,
        person: Option<&InternalPerson>,
        vehicle: Option<&InternalVehicle>,
    ) -> Disutility {
        self.travel_time(link, departure_time, person, vehicle)
            .as_secs_f64()
            * 1.0
    }
    fn get_link_min_travel_disutility(&self, link: &Link) -> Disutility {
        self.travel_disutility(link, SimTime::from_secs(0), None, None)
    }
}

fn travel_time(length: f64, speed: f64) -> Duration {
    if length < 10e-10 {
        return Duration::ZERO;
    }
    let duration = length / speed;
    match duration.classify() {
        FpCategory::Nan | FpCategory::Infinite => Duration::MAX,
        FpCategory::Normal | FpCategory::Subnormal | FpCategory::Zero => {
            Duration::from_secs_f64(duration)
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::simulation::InternalAttributes;
    use crate::simulation::config::{AgentParameter, Config, ModeParameter, Scoring};
    use crate::simulation::events::{LinkEnterEvent, LinkLeaveEvent, VehicleEntersTrafficEvent};
    use crate::simulation::id::Id;
    use crate::simulation::io::xml::attributes::{IOAttribute, IOAttributes};
    use crate::simulation::io::xml::population::IOPerson;
    use crate::simulation::replanning::routing::cost::{
        FreeOrMaxSpeedTravelTimeAndDisutility, ScoringBasedTravelTimeAndDisutility,
        TravelDisutility, TravelTime,
    };
    use crate::simulation::replanning::routing::graph::Graph;
    use crate::simulation::replanning::routing::graph::tests::{
        get_triangle_test_network, net_to_graph,
    };
    use crate::simulation::replanning::routing::travel_time_calculator::test::{link, network};
    use crate::simulation::replanning::routing::travel_time_calculator::{
        GlobalTravelTimeCalculator, PartitionTravelTimeCollector,
    };
    use crate::simulation::scenario::network::Link;
    use crate::simulation::scenario::population::{InternalPerson, SUBPOPULATION};
    use crate::simulation::scenario::vehicles::InternalVehicle;
    use crate::simulation::time::SimTime;
    use macros::deterministic_id_test;
    use std::sync::Arc;
    use std::time::Duration;

    fn mode_params(mode: &str, traveling: f64, distance: f64) -> ModeParameter {
        ModeParameter {
            mode: mode.to_string(),
            marginal_utility_of_traveling: traveling,
            marginal_utility_of_distance: distance,
            monetary_distance_cost_rate: 0.0,
            daily_money_constant: 0.0,
            daily_utility_constant: 0.0,
            constant: 0.0,
        }
    }

    fn scoring_config() -> Config {
        let mut config = Config::default();
        config.set_scoring(Scoring {
            write_experienced_plans: false,
            activity_params: Vec::new(),
            mode_params: vec![
                mode_params("car", -6.0, -0.01),
                mode_params("walk", -3.0, 0.0),
            ],
            agent_params: vec![
                AgentParameter::default(),
                AgentParameter {
                    subpopulation: "freight".to_string(),
                    performing: 2.0,
                    ..AgentParameter::default()
                },
            ],
        });
        config
    }

    fn global_travel_time(
        mut partitions: Vec<PartitionTravelTimeCollector>,
        links: &[Link],
    ) -> Arc<GlobalTravelTimeCalculator> {
        let net = network(links);
        let global = Arc::new(GlobalTravelTimeCalculator::new(
            partitions.len().max(1),
            Duration::from_secs(10),
            Duration::from_secs(100),
        ));
        for (rank, partition) in partitions.iter_mut().enumerate() {
            global.submit(0, rank as u32, partition.finish(&net));
        }
        global
    }

    fn person(id: &str, subpopulation: &str) -> InternalPerson {
        IOPerson {
            id: id.to_string(),
            plans: Vec::new(),
            attributes: Some(IOAttributes {
                attributes: vec![IOAttribute::new_with_class(
                    SUBPOPULATION.to_string(),
                    "java.lang.String".to_string(),
                    subpopulation.to_string(),
                )],
            }),
        }
        .into()
    }

    fn assert_close(expected: f64, actual: f64) {
        assert!(
            (expected - actual).abs() < 1e-12,
            "expected {expected}, got {actual}"
        );
    }

    fn observe_travel_time(
        partition: &mut PartitionTravelTimeCollector,
        mode: &str,
        link: &Id<Link>,
        vehicle: &str,
        travel_time: u64,
    ) {
        let vehicle = Id::create(vehicle);
        partition.process_vehicle_enters_traffic_event(&VehicleEntersTrafficEvent {
            time: SimTime::from_secs(0),
            vehicle: vehicle.clone(),
            link: link.clone(),
            person: Id::create(format!("{mode}-person").as_str()),
            network_mode: Id::create(mode),
            relative_position: 1.0,
            attributes: InternalAttributes::default(),
        });
        partition.process_link_enter_event(&LinkEnterEvent {
            time: SimTime::from_secs(0),
            link: link.clone(),
            vehicle: vehicle.clone(),
            attributes: InternalAttributes::default(),
        });
        partition.process_link_leave_event(&LinkLeaveEvent {
            time: SimTime::from_secs(travel_time),
            link: link.clone(),
            vehicle,
            attributes: InternalAttributes::default(),
        });
    }

    /// Test the FreeOrMaxSpeedTravelTimeAndDisutility implementation of TravelTime and TravelDisutility
    #[deterministic_id_test]
    fn test_free_or_max_speed_travel_time_and_disutility() {
        let fomsttad = FreeOrMaxSpeedTravelTimeAndDisutility;

        let network = get_triangle_test_network();
        let graph = net_to_graph(&network);

        let link = graph.edge(Id::create("4")).unwrap();

        assert_eq!(
            fomsttad.travel_time(link, SimTime::from_secs(0), None, None),
            Duration::from_secs(4)
        );
        assert_eq!(
            fomsttad.travel_disutility(link, SimTime::from_secs(0), None, None),
            4.0
        );

        // also test that the vehicle's max speed is respected

        // vehicle max_v is lower than freespeed, so travel time will be longer
        let vehicle = InternalVehicle::new(0, 0, 1000.0, 0.0);

        assert_eq!(
            fomsttad.travel_time(link, SimTime::from_secs(0), None, Some(&vehicle)),
            Duration::from_secs(10)
        );
        assert_eq!(
            fomsttad.travel_disutility(link, SimTime::from_secs(0), None, Some(&vehicle)),
            10.0
        );
    }

    // checks if the correct travel times and disutilities are used for different modes, even when no vehicle is provided
    #[deterministic_id_test]
    fn scoring_costs_use_router_mode_without_vehicle() {
        let config = scoring_config();
        let mut partition =
            PartitionTravelTimeCollector::new(Duration::from_secs(10), Duration::from_secs(100));
        let link = link("mode-specific", 100.0, 10.0);
        observe_travel_time(&mut partition, "car", &link.id, "car-vehicle", 20);
        observe_travel_time(&mut partition, "walk", &link.id, "walk-vehicle", 30);
        let travel_time = global_travel_time(vec![partition], &[link.clone()]);

        let car = ScoringBasedTravelTimeAndDisutility::new(
            &config,
            Id::create("car"),
            travel_time.clone(),
        );
        let walk =
            ScoringBasedTravelTimeAndDisutility::new(&config, Id::create("walk"), travel_time);
        let person = person("default-person", "person");

        assert_eq!(
            Duration::from_secs(20),
            car.travel_time(&link, SimTime::from_secs(0), None, None)
        );
        assert_eq!(
            Duration::from_secs(30),
            walk.travel_time(&link, SimTime::from_secs(0), None, None)
        );
        assert_close(
            1.0 + 20.0 * 12.0 / 3600.0,
            car.travel_disutility(&link, SimTime::from_secs(0), Some(&person), None),
        );
        assert_close(
            30.0 * 9.0 / 3600.0,
            walk.travel_disutility(&link, SimTime::from_secs(0), Some(&person), None),
        );
    }

    #[deterministic_id_test]
    #[should_panic(expected = "No scoring parameters configured for network mode freight")]
    fn scoring_costs_require_parameters_for_router_mode() {
        let config = scoring_config();
        ScoringBasedTravelTimeAndDisutility::new(
            &config,
            Id::create("freight"),
            global_travel_time(Vec::new(), &[]),
        );
    }

    #[deterministic_id_test]
    #[should_panic(expected = "No scoring parameters configured for subpopulation missing")]
    fn scoring_costs_require_parameters_for_person_subpopulation() {
        let config = scoring_config();
        let costs = ScoringBasedTravelTimeAndDisutility::new(
            &config,
            Id::create("car"),
            global_travel_time(Vec::new(), &[]),
        );
        let link = link("missing-subpopulation", 100.0, 10.0);
        let person = person("missing-person", "missing");

        costs.travel_disutility(&link, SimTime::from_secs(0), Some(&person), None);
    }
}
