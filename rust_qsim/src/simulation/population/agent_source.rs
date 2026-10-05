use crate::simulation::agents::agent::SimulationAgent;
use crate::simulation::config::{Config, RoutingMode};
use crate::simulation::id::Id;
use crate::simulation::scenario::population::InternalPerson;
use crate::simulation::scenario::{MobsimScenarioPartition, PopulationShard};
use ahash::HashMapExt;
use nohash_hasher::IntMap;
use std::sync::Arc;

// needs to be an Arc since agents are inserted on every partition.
pub type DynAgentSource = Arc<dyn AgentSource + Send + Sync>;
pub type AgentSet = IntMap<Id<InternalPerson>, SimulationAgent>;

pub trait AgentSource {
    fn create_agents(
        &self,
        population: PopulationShard,
        partition: &MobsimScenarioPartition,
    ) -> AgentSet;
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
        population: PopulationShard,
        _partition: &MobsimScenarioPartition,
    ) -> AgentSet {
        let persons = population.population.persons;
        let mut agents = IntMap::with_capacity(persons.len());

        for (id, person) in persons {
            agents.insert(id, SimulationAgent::new_plan_based(person));
        }
        agents
    }
}

pub struct PreplanningHorizonAgentSource;

impl AgentSource for PreplanningHorizonAgentSource {
    fn create_agents(
        &self,
        population: PopulationShard,
        partition: &MobsimScenarioPartition,
    ) -> AgentSet {
        let persons = population.population.persons;
        let mut agents = IntMap::with_capacity(persons.len());

        for (id, person) in persons {
            identify_logic_and_insert(&mut agents, id, person, &partition.scenario.config);
        }
        agents
    }
}

fn identify_logic_and_insert(
    agents: &mut IntMap<Id<InternalPerson>, SimulationAgent>,
    id: Id<InternalPerson>,
    person: InternalPerson,
    config: &Config,
) {
    if config.routing().mode == RoutingMode::UsePlans {
        agents.insert(id, SimulationAgent::new_plan_based(person));
        return;
    }

    // go through all attributes of person's legs and check whether there is some marked as rolling horizon logic
    let has_at_least_one_preplanning_horizon = person
        .selected_plan()
        .as_ref()
        .unwrap_or_else(|| panic!("Plan does not exist for person with id: {}", id.external()))
        .acts()
        .iter()
        .any(|l| {
            l.attributes
                .attributes
                .contains_key(crate::simulation::scenario::population::PREPLANNING_HORIZON)
        });

    if has_at_least_one_preplanning_horizon {
        agents.insert(id, SimulationAgent::new_adaptive_plan_based(person));
    } else {
        // if there is no rolling horizon logic, we assume that the person has a plan logic
        // and we create a InternalSimulationAgent with plan logic
        agents.insert(id, SimulationAgent::new_plan_based(person));
    }
}

#[cfg(test)]
mod tests {
    use super::{AgentSet, AgentSource, DynAgentSource, IntoDynAgentSource, PopulationAgentSource};
    use crate::simulation::config::Config;
    use crate::simulation::id::Id;
    use crate::simulation::network::signals::Signals;
    use crate::simulation::network::sim_network::SimNetworkPartition;
    use crate::simulation::scenario::network::{Link, Network};
    use crate::simulation::scenario::population::{
        InternalActivity, InternalPerson, InternalPlan, Population,
    };
    use crate::simulation::scenario::vehicles::Garage;
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
            _population: PopulationShard,
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
        let shard = PopulationShard { population };

        let agents = PopulationAgentSource.create_agents(shard, &core);

        assert_eq!(1, agents.len());
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
                garage: Arc::new(Garage::default()),
                transit_schedule: Arc::new(
                    crate::simulation::scenario::transit::TransitSchedule::default(),
                ),
                config,
                signals: Arc::new(Signals::default()),
            },
            network_partition,
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
