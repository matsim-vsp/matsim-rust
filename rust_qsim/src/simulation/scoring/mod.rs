use crate::simulation::config::{Config, ScoringPlansCollectionType};
use crate::simulation::events::EventHandlerRegisterFn;
use crate::simulation::framework_events::{
    MobsimListenerRegisterFn, PartitionListenerRegisterFn, QSimId,
};
use crate::simulation::id::Id;
use crate::simulation::io;
use crate::simulation::scenario::ScenarioPartition;
use crate::simulation::scenario::population::InternalPerson;
use crate::simulation::scoring::backpacking::backpacking_scoring_engine::BackpackingScoringEngine;
use crate::simulation::scoring::homesending::homesending_scoring_engine::HomesendingScoringEngine;
use crate::simulation::scoring::mapping::mapping_scoring_engine::MappingCollectorEngine;
use crate::simulation::scoring::mapping::mapping_scoring_engine::MappingForwardingEngine;
use crate::simulation::scoring::mapping::{person_hash, vehicle_hash};
use nohash_hasher::IntMap;
use std::any::Any;
use std::sync::Arc;
use std::sync::mpsc::{Sender, channel};
use std::thread;
use tracing::info;

pub mod backpacking;
pub mod homesending;
pub mod mapping;
pub mod partial_plans;

pub trait Message: Any + Send {
    fn as_any(&self) -> &dyn Any;

    fn into_any(self: Box<Self>) -> Box<dyn Any>;
}

impl<T: Any + Send> Message for T {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
}

pub struct InternalScoringMessage {
    // time: u32,
    from_process: QSimId,
    #[allow(unused)]
    to_process: QSimId,
    message: Box<dyn Message>,
}

/// Trait for a scoring engine that can be initialized and finished by the controller.
pub trait ScoringEngine: Send + Sync {
    /// Attaches the senders to the internal structs managing message handling.
    fn attach_senders(&mut self, senders: Vec<Sender<InternalScoringMessage>>);

    /// Returns the register functions, given to the Partitions
    fn register_fn(
        &self,
    ) -> (
        Box<EventHandlerRegisterFn>,
        Box<PartitionListenerRegisterFn>,
        Box<MobsimListenerRegisterFn>,
    );

    /// Called from the Controller after the mobsim is finished. Shall finish remaining tasks,
    /// that can only be done after the iteration end.
    fn finish(&self);

    /// Actual scoring.
    fn scoring(&self);
}

/// Initializing function, creating n ScoringEngines of the type, set in the config
pub fn create_for_n_partitions(
    partitions: &Vec<Option<ScenarioPartition>>,
    config: Arc<Config>,
) -> (
    Vec<Box<dyn ScoringEngine>>,
    Vec<Box<EventHandlerRegisterFn>>,
    Vec<Box<PartitionListenerRegisterFn>>,
    Vec<Box<MobsimListenerRegisterFn>>,
) {
    info!(
        "Initializing Scoring with {:?}...",
        config.scoring().plans_collection_type
    );

    let num_parts = config.partitioning().num_parts;
    let num_collectors = config.scoring().collector_threads;

    // Create ScoringEngines with channels
    let mut senders: Vec<_> = Vec::new();
    let mut scorings: Vec<_> = Vec::new();

    for rank in 0..num_parts {
        let partition = partitions.get(rank as usize).unwrap().as_ref().unwrap();

        let (sender, receiver) = channel();

        let scoring: Box<dyn ScoringEngine> = match config.scoring().plans_collection_type {
            ScoringPlansCollectionType::Backpacking => Box::new(BackpackingScoringEngine::new(
                rank,
                &partition.population,
                receiver,
                vec![],
                io::resolve_path(config.context(), &config.output().output_dir),
            )),
            ScoringPlansCollectionType::Mapping => Box::new(MappingForwardingEngine::new(
                rank,
                person_hash(num_collectors),
                vehicle_hash(num_collectors),
                num_parts as usize,
                num_collectors as usize,
                config.scoring().sync_interval,
                receiver,
                vec![],
                io::resolve_path(config.context(), &config.output().output_dir),
            )),
            ScoringPlansCollectionType::HomeSending => {
                // Prepare person_id2home_partition map needed for Homesending
                let mut person_id2home_partition: IntMap<Id<InternalPerson>, QSimId> =
                    IntMap::default();
                for (i, partition) in partitions.iter().enumerate() {
                    let partition = partition.as_ref().unwrap();

                    for person in partition.population.persons.keys() {
                        person_id2home_partition.insert(person.clone(), i as QSimId);
                    }
                }
                Box::new(HomesendingScoringEngine::new(
                    rank,
                    &partition.population,
                    num_parts as usize,
                    person_id2home_partition.clone(),
                    receiver,
                    vec![],
                    io::resolve_path(config.context(), &config.output().output_dir),
                ))
            }
        };

        senders.push(sender);
        scorings.push(scoring);
    }

    let mut collectors = Vec::new();
    if config.scoring().plans_collection_type == ScoringPlansCollectionType::Mapping {
        for i in 0..num_collectors {
            // Prepare person_id2home_partition map needed for Homesending
            let mut person_id2home_partition: IntMap<Id<InternalPerson>, QSimId> =
                IntMap::default();
            for (i, partition) in partitions.iter().enumerate() {
                let partition = partition.as_ref().unwrap();

                for person in partition.population.persons.keys() {
                    person_id2home_partition.insert(person.clone(), i as QSimId);
                }
            }

            let (sender, receiver) = channel();

            collectors.push(MappingCollectorEngine::new(
                i + num_parts,
                person_hash(num_collectors),
                num_parts as usize,
                num_collectors as usize,
                person_id2home_partition.clone(),
                receiver,
                vec![],
            ));

            senders.push(sender);
        }

        for (i, mut collector) in collectors.drain(..).enumerate() {
            collector.attach_senders(senders.clone());

            thread::Builder::new()
                .name(format!("scoring-{i}"))
                .spawn(move || collector.work())
                .unwrap();
        }

        for mut collector in collectors {
            collector.attach_senders(senders.clone());
        }
    }

    let mut event_register_functions = Vec::new();
    let mut partition_register_functions = Vec::new();
    let mut mobsim_register_functions = Vec::new();

    for scoring in scorings.iter_mut() {
        scoring.attach_senders(senders.clone());

        let (event_fn, partition_fn, mobsim_fn) = scoring.register_fn();
        event_register_functions.push(event_fn);
        partition_register_functions.push(partition_fn);
        mobsim_register_functions.push(mobsim_fn);
    }

    (
        scorings,
        event_register_functions,
        partition_register_functions,
        mobsim_register_functions,
    )
}
