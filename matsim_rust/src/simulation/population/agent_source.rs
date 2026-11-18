use crate::simulation::agents::agent::ActivityAgent;
use crate::simulation::agents::agent_logic::{
    AdaptivePlanBasedSimulationLogic, PlanBasedSimulationLogic,
};
use crate::simulation::id::Id;
use crate::simulation::pt::driver::WaitingDriver;
use crate::simulation::scenario::population::InternalPerson;
use crate::simulation::scenario::{MobsimScenarioPartition, PopulationShard};
use ahash::HashMapExt;
use nohash_hasher::IntMap;
use std::sync::Arc;

// needs to be an Arc since agents are inserted on every partition.
pub type DynAgentSource = Arc<dyn AgentSource + Send + Sync>;
pub type AgentSet = IntMap<Id<InternalPerson>, ActivityAgent>;

/// Creates agents for a mobsim partition. A mobsim asks several sources one after the other. Each
/// source takes the persons of `population` for which it creates agents, so that every person ends
/// up with exactly one agent.
pub trait AgentSource {
    fn create_agents(
        &self,
        population: &mut PopulationShard,
        partition: &MobsimScenarioPartition,
    ) -> AgentSet;
}

/// The agent sources of a mobsim by default: the persons of the population.
pub fn default_agent_sources() -> Vec<DynAgentSource> {
    vec![Arc::new(PopulationAgentSource)]
}

pub trait IntoDynAgentSource {
    fn into_dyn_agent_source(self) -> DynAgentSource;
}

impl<T> IntoDynAgentSource for T
where
    T: AgentSource + Send + Sync + 'static,
{
    fn into_dyn_agent_source(self) -> DynAgentSource {
        Arc::new(self)
    }
}

impl<T> IntoDynAgentSource for Arc<T>
where
    T: AgentSource + Send + Sync + 'static,
{
    fn into_dyn_agent_source(self) -> DynAgentSource {
        self
    }
}

impl IntoDynAgentSource for DynAgentSource {
    fn into_dyn_agent_source(self) -> DynAgentSource {
        self
    }
}

pub struct PopulationAgentSource;

impl AgentSource for PopulationAgentSource {
    fn create_agents(
        &self,
        population: &mut PopulationShard,
        _partition: &MobsimScenarioPartition,
    ) -> AgentSet {
        let persons = std::mem::take(&mut population.population.persons);
        let mut agents = IntMap::with_capacity(persons.len());

        for (id, person) in persons {
            agents.insert(id, PlanBasedSimulationLogic::new_agent(person));
        }
        agents
    }
}

pub struct PreplanningHorizonAgentSource;

impl AgentSource for PreplanningHorizonAgentSource {
    fn create_agents(
        &self,
        population: &mut PopulationShard,
        _partition: &MobsimScenarioPartition,
    ) -> AgentSet {
        let persons = std::mem::take(&mut population.population.persons);
        let mut agents = IntMap::with_capacity(persons.len());

        for (id, person) in persons {
            let plan = person.selected_plan().unwrap_or_else(|| {
                panic!("Plan does not exist for person with id: {}", id.external())
            });
            let agent = if AdaptivePlanBasedSimulationLogic::applies_to(plan) {
                AdaptivePlanBasedSimulationLogic::new_agent(person)
            } else {
                // Persons without a rolling horizon follow their selected plan.
                PlanBasedSimulationLogic::new_agent(person)
            };
            agents.insert(id, agent);
        }
        agents
    }
}

/// Creates the drivers of the transit departures whose first stop is on the partition. Each waits
/// in an activity until its departure, like the drivers that MATSim's transit engines insert. With
/// teleported transit, the schedule is empty and there are no drivers.
pub struct TransitDriverAgentSource;

impl AgentSource for TransitDriverAgentSource {
    fn create_agents(
        &self,
        _population: &mut PopulationShard,
        partition: &MobsimScenarioPartition,
    ) -> AgentSet {
        let schedule = &partition.transit_sim_schedule;
        schedule
            .departures_starting_on(partition.rank)
            .map(|index| {
                (
                    schedule.driver(index).clone(),
                    WaitingDriver::new_agent(schedule.clone(), index),
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{AgentSet, AgentSource, DynAgentSource, IntoDynAgentSource, PopulationAgentSource};
    use crate::simulation::config::Config;
    use crate::simulation::id::Id;
    use crate::simulation::network::sim_network::SimNetworkPartition;
    use crate::simulation::scenario::network::{Link, Network};
    use crate::simulation::scenario::population::{
        InternalActivity, InternalPerson, InternalPlan, Population,
    };
    use crate::simulation::scenario::{
        Coordinate, MobsimScenarioPartition, PopulationShard, ScenarioCore,
    };
    use macros::deterministic_id_test;
    use nohash_hasher::IntMap;
    use std::sync::Arc;

    struct TestAgentSource;

    impl AgentSource for TestAgentSource {
        fn create_agents(
            &self,
            _population: &mut PopulationShard,
            _partition: &MobsimScenarioPartition,
        ) -> AgentSet {
            IntMap::default()
        }
    }

    #[test]
    fn converts_concrete_agent_source_to_dyn_agent_source() {
        let source = TestAgentSource.into_dyn_agent_source();

        assert_eq!(Arc::strong_count(&source), 1);
    }

    #[test]
    fn converts_arc_agent_source_to_dyn_agent_source() {
        let source = Arc::new(TestAgentSource);
        let dyn_source = source.clone().into_dyn_agent_source();

        assert_eq!(Arc::strong_count(&source), 2);
        assert_eq!(Arc::strong_count(&dyn_source), 2);
    }

    #[test]
    fn accepts_existing_dyn_agent_source() {
        let source: DynAgentSource = Arc::new(TestAgentSource);
        let dyn_source = source.clone().into_dyn_agent_source();

        assert_eq!(Arc::strong_count(&source), 2);
        assert_eq!(Arc::strong_count(&dyn_source), 2);
    }

    #[deterministic_id_test]
    fn population_agent_source_consumes_owned_population_shard() {
        let core = empty_partition_core();
        let population = Population::from_persons(vec![person("person-1")]);
        let mut shard = PopulationShard { population };

        let agents = PopulationAgentSource.create_agents(&mut shard, &core);

        assert_eq!(1, agents.len());
        assert!(shard.population.persons.is_empty());
        assert!(agents.contains_key(&Id::get_from_ext("person-1")));
    }

    fn empty_partition_core() -> MobsimScenarioPartition {
        let config = Arc::new(Config::default());
        let network = Arc::new(Network::new());
        let network_partition = SimNetworkPartition::from_network_for_test(&network, 0, &config);
        MobsimScenarioPartition {
            rank: 0,
            scenario: ScenarioCore {
                network,
                facilities: Arc::new(
                    crate::simulation::scenario::facilities::ActivityFacilities::default(),
                ),
                config,
                ..ScenarioCore::default()
            },
            network_partition,
            transit_sim_schedule: Arc::default(),
        }
    }

    fn person(id: &str) -> InternalPerson {
        let mut plan = InternalPlan::default();
        plan.add_act(InternalActivity::new(
            Some(Coordinate::default()),
            "home",
            Id::<Link>::create("link"),
            None,
            None,
            None,
        ));
        InternalPerson::new(Id::create(id), plan)
    }
}
