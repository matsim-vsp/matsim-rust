use crate::simulation::events::{
    EventTrait, EventsManager, PersonEntersVehicleEvent, PersonLeavesVehicleEvent,
};
use crate::simulation::framework_events::{
    MobsimEvent, MobsimEventsManager, PartitionEvent, PartitionEventsManager, QSimId, RuntimeEvent,
    WorkerListenerRegisterFunction,
};
use crate::simulation::id::Id;
use crate::simulation::scenario::population::{InternalPerson, Population};
use crate::simulation::scoring::backpacking::backpacking_data_collector::BackpackingDataCollector;
use crate::simulation::scoring::backpacking::backpacking_message_broker::BackpackingMessageBroker;
use crate::simulation::scoring::{InternalScoringMessage, ScoringEngine};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

pub struct BackpackingScoringEngine {
    backpacking_data_collector: Arc<Mutex<BackpackingDataCollector>>,
    backpacking_message_broker: Arc<Mutex<BackpackingMessageBroker>>,
}

impl BackpackingScoringEngine {
    pub fn new(
        rank: QSimId,
        home_person_ids: Vec<Id<InternalPerson>>,
        receiver: Receiver<InternalScoringMessage>,
        senders: Vec<Sender<InternalScoringMessage>>,
    ) -> Self {
        let backpacking_message_broker = BackpackingMessageBroker::new(receiver, senders, rank);
        let backpacking_data_collector = BackpackingDataCollector::new(
            home_person_ids,
            rank,
            Arc::clone(&backpacking_message_broker),
        );

        Self {
            backpacking_data_collector,
            backpacking_message_broker,
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

    fn register_fn(&self) -> Box<WorkerListenerRegisterFunction> {
        let data_collector = self.backpacking_data_collector.clone();
        let message_broker = self.backpacking_message_broker.clone();

        Box::new(move |events, mobsim_events, partition_events| {
            Self::register_event_handlers(data_collector.clone(), events);
            Self::register_partition_listeners(
                data_collector.clone(),
                message_broker.clone(),
                partition_events,
            );
            Self::register_mobsim_listeners(data_collector, message_broker, mobsim_events);
        })
    }

    fn finish(&self) -> Population {
        self.backpacking_data_collector.lock().unwrap().finish()
    }

    fn scoring(&self) {
        // TODO
    }
}

impl BackpackingScoringEngine {
    fn register_event_handlers(
        data_collector: Arc<Mutex<BackpackingDataCollector>>,
        events: &mut EventsManager,
    ) {
        let reset_data_collector = Arc::clone(&data_collector);
        events.on_reset_iteration(move |_| {
            reset_data_collector.lock().unwrap().reset_iteration();
        });

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
            let remove_vehicle = bdc
                .get_vehicles_mut()
                .get_mut(&e.vehicle)
                .map(|persons| {
                    persons.remove(&e.person);
                    persons.is_empty()
                })
                .unwrap_or(false);
            if remove_vehicle {
                bdc.get_vehicles_mut().remove(&e.vehicle);
            }
        });
    }

    fn register_partition_listeners(
        data_collector: Arc<Mutex<BackpackingDataCollector>>,
        message_broker: Arc<Mutex<BackpackingMessageBroker>>,
        events: &mut PartitionEventsManager,
    ) {
        let data_collector1 = Arc::clone(&data_collector);
        let message_broker1 = Arc::clone(&message_broker);
        events.on_event(move |e: &RuntimeEvent<PartitionEvent>| match &e.payload {
            PartitionEvent::VehicleLeavesPartition(i) => {
                let mut bdc = data_collector1.lock().unwrap();
                let mut bmb = message_broker1.lock().unwrap();

                let leaving_vehicle = bdc.remove_leaving_vehicles(&i.vehicle_id);
                bmb.add_leaving_vehicle(i.to, i.vehicle_id.clone(), leaving_vehicle);
            }
            PartitionEvent::AgentLeavesPartition(i) => {
                let mut bdc = data_collector1.lock().unwrap();
                let mut bmb = message_broker1.lock().unwrap();

                let leaving_backpack = bdc.remove_leaving_backpack(&i.agent_id);
                bmb.add_leaving_backpack(i.to, i.agent_id.clone(), leaving_backpack);
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
    }

    fn register_mobsim_listeners(
        data_collector: Arc<Mutex<BackpackingDataCollector>>,
        message_broker: Arc<Mutex<BackpackingMessageBroker>>,
        events: &mut MobsimEventsManager,
    ) {
        let data_collector1 = Arc::clone(&data_collector);
        let message_broker1 = Arc::clone(&message_broker);

        events.on_event(move |e: &RuntimeEvent<MobsimEvent>| match &e.payload {
            MobsimEvent::BeforeSimStep(_) | MobsimEvent::BeforeCleanup => {
                let mut bdc = data_collector1.lock().unwrap();
                bdc.drain_scoring_messages();
                bdc.replay_deferred_link_events();
            }
            MobsimEvent::AfterSimStep(_) => {
                message_broker1.lock().unwrap().send();
            }
            _ => {}
        });
    }
}
