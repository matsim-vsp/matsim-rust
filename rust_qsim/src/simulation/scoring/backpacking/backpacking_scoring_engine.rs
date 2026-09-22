use crate::simulation::events::{
    EventHandlerRegisterFn, EventTrait, EventsManager, PersonEntersVehicleEvent,
    PersonLeavesVehicleEvent,
};
use crate::simulation::framework_events::{
    MobsimEvent, MobsimEventsManager, MobsimListenerRegisterFn, PartitionEvent,
    PartitionEventsManager, PartitionListenerRegisterFn, QSimId, RuntimeEvent,
};
use crate::simulation::scenario::population::Population;
use crate::simulation::scoring::backpacking::backpacking_data_collector::BackpackingDataCollector;
use crate::simulation::scoring::backpacking::backpacking_message_broker::BackpackingMessageBroker;
use crate::simulation::scoring::{InternalScoringMessage, ScoringEngine};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use tracing::info;

pub struct BackpackingScoringEngine {
    backpacking_data_collector: Arc<Mutex<BackpackingDataCollector>>,
    backpacking_message_broker: Arc<Mutex<BackpackingMessageBroker>>,
    rank: QSimId,
    output_path: PathBuf,
}

impl BackpackingScoringEngine {
    pub(crate) fn new(
        rank: QSimId,
        population: &Population,
        receiver: Receiver<InternalScoringMessage>,
        senders: Vec<Sender<InternalScoringMessage>>,
        output_path: PathBuf,
    ) -> Self {
        let backpacking_message_broker = BackpackingMessageBroker::new(receiver, senders, rank);
        let backpacking_data_collector = BackpackingDataCollector::new(
            population,
            rank,
            Arc::clone(&backpacking_message_broker),
        );

        Self {
            backpacking_data_collector,
            backpacking_message_broker,
            rank,
            output_path,
        }
    }
}

impl ScoringEngine for BackpackingScoringEngine {
    fn attach_senders(&mut self, senders: Vec<Sender<InternalScoringMessage>>) {
        self.backpacking_data_collector
            .lock()
            .unwrap()
            .attach_senders(senders);
    }

    fn register_fn(
        &self,
    ) -> (
        Box<EventHandlerRegisterFn>,
        Box<PartitionListenerRegisterFn>,
        Box<MobsimListenerRegisterFn>,
    ) {
        (
            Self::register_event_fn(self.backpacking_data_collector.clone()),
            Self::register_partition_fn(
                self.backpacking_data_collector.clone(),
                self.backpacking_message_broker.clone(),
            ),
            Self::register_mobsim_fn(
                self.backpacking_data_collector.clone(),
                self.backpacking_message_broker.clone(),
            ),
        )
    }

    fn finish(&self) {
        let population = self.backpacking_data_collector.lock().unwrap().finish();
        let mut o = self.output_path.clone();
        o.push(format!("plans/output_plans_{}.binpb", self.rank));
        info!("Starting writing PartitionPlans to {:?}", o);
        population.to_file(o.as_path());
        info!("Finished writing PartitionPlans to {:?}", o);
    }

    fn scoring(&self) {
        // TODO
    }
}

impl BackpackingScoringEngine {
    pub(crate) fn register_event_fn(
        data_collector: Arc<Mutex<BackpackingDataCollector>>,
    ) -> Box<EventHandlerRegisterFn> {
        Box::new(move |events: &mut EventsManager| {
            // General backpacking event forwarding
            let data_collector1 = Arc::clone(&data_collector);
            events.on_any(move |e: &dyn EventTrait| {
                let mut bdc = data_collector1.lock().unwrap();
                bdc.handle_event(e);
            });

            // Events for Vehicle2Person mappings
            let data_collector2 = Arc::clone(&data_collector);
            events.on::<PersonEntersVehicleEvent, _>(move |e: &PersonEntersVehicleEvent| {
                let mut bdc = data_collector2.lock().unwrap();
                bdc.get_vehicles_mut()
                    .entry(e.vehicle.clone())
                    .or_default()
                    .insert(e.person.clone());
            });

            let data_collector3 = Arc::clone(&data_collector);
            events.on::<PersonLeavesVehicleEvent, _>(move |e: &PersonLeavesVehicleEvent| {
                let mut bdc = data_collector3.lock().unwrap();
                bdc.get_vehicles_mut().remove(&e.vehicle);
            });
        })
    }

    pub(crate) fn register_partition_fn(
        data_collector: Arc<Mutex<BackpackingDataCollector>>,
        message_broker: Arc<Mutex<BackpackingMessageBroker>>,
    ) -> Box<PartitionListenerRegisterFn> {
        Box::new(move |events: &mut PartitionEventsManager| {
            let data_collector1 = Arc::clone(&data_collector);
            let message_broker1 = Arc::clone(&message_broker);
            events.on_event(move |e: &RuntimeEvent<PartitionEvent>| match &e.payload {
                PartitionEvent::VehicleLeavesPartition(i) => {
                    let mut bdc = data_collector1.lock().unwrap();
                    let mut bmb = message_broker1.lock().unwrap();

                    let leaving_vehicle = bdc.remove_leaving_vehicles(&i.vehicle_id);
                    bmb.add_leaving_vehicle(i.to.clone(), i.vehicle_id.clone(), leaving_vehicle);
                }
                PartitionEvent::AgentLeavesPartition(i) => {
                    let mut bdc = data_collector1.lock().unwrap();
                    let mut bmb = message_broker1.lock().unwrap();

                    let leaving_backpack = bdc.remove_leaving_backpack(&i.agent_id);
                    bmb.add_leaving_backpack(i.to.clone(), i.agent_id.clone(), leaving_backpack);
                }
                PartitionEvent::AgentEntersPartition(i) => {
                    message_broker1
                        .lock()
                        .unwrap()
                        .wait_for_backpack(i.agent_id.clone());
                }
                PartitionEvent::VehicleEntersPartition(i) => {
                    let mut bdc = data_collector1.lock().unwrap();
                    let mut bmb = message_broker1.lock().unwrap();

                    bdc.get_pending_vehicles_mut().insert(i.vehicle_id.clone());
                    bmb.wait_for_vehicle(i.vehicle_id.clone());
                }
            });
        })
    }

    pub(crate) fn register_mobsim_fn(
        data_collector: Arc<Mutex<BackpackingDataCollector>>,
        message_broker: Arc<Mutex<BackpackingMessageBroker>>,
    ) -> Box<MobsimListenerRegisterFn> {
        Box::new(move |events: &mut MobsimEventsManager| {
            let data_collector1 = Arc::clone(&data_collector);
            let message_broker1 = Arc::clone(&message_broker);

            events.on_event(move |e: &RuntimeEvent<MobsimEvent>| match &e.payload {
                MobsimEvent::BeforeSimStep(_) => {
                    let mut bdc = data_collector1.lock().unwrap();
                    bdc.drain_scoring_messages();
                    bdc.replay_deferred_link_events();
                }
                MobsimEvent::AfterSimStep(_) => {
                    message_broker1.lock().unwrap().send();
                }
                _ => {}
            });
        })
    }
}
