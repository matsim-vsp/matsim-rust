use crate::simulation::events::{
    EventHandlerRegisterFn, EventTrait, EventsManager, PersonEntersVehicleEvent,
    PersonLeavesVehicleEvent,
};
use crate::simulation::framework_events::{
    MobsimEvent, MobsimEventsManager, MobsimListenerRegisterFn, PartitionEvent,
    PartitionListenerRegisterFn, QSimId, RuntimeEvent,
};
use crate::simulation::id::Id;
use crate::simulation::scenario::population::{InternalPerson, Population};
use crate::simulation::scoring::homesending::homesending_data_collector::HomeSendingDataCollector;
use crate::simulation::scoring::homesending::homesending_message_broker::HomeSendingMessageBroker;
use crate::simulation::scoring::{InternalScoringMessage, ScoringEngine};
use nohash_hasher::IntMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use tracing::info;

pub struct HomesendingScoringEngine {
    homesending_data_collector: Arc<Mutex<HomeSendingDataCollector>>,
    homesending_message_broker: Arc<Mutex<HomeSendingMessageBroker>>,
    rank: QSimId,

    output_path: PathBuf,
}

impl HomesendingScoringEngine {
    pub(crate) fn new(
        rank: QSimId,
        population: &Population,
        num_partitions: usize,
        person_id2_partition_id: IntMap<Id<InternalPerson>, QSimId>,
        receiver: Receiver<InternalScoringMessage>,
        senders: Vec<Sender<InternalScoringMessage>>,
        output_path: PathBuf,
    ) -> Self {
        let homesending_message_broker =
            HomeSendingMessageBroker::new(receiver, senders, num_partitions, rank, population);
        let homesending_data_collector = HomeSendingDataCollector::new(
            population,
            person_id2_partition_id,
            rank,
            Arc::clone(&homesending_message_broker),
        );
        HomeSendingMessageBroker::init(
            &homesending_message_broker,
            Arc::downgrade(&homesending_data_collector),
        );

        Self {
            homesending_data_collector,
            homesending_message_broker,
            rank,
            output_path,
        }
    }
}

impl ScoringEngine for HomesendingScoringEngine {
    fn attach_senders(&mut self, senders: Vec<Sender<InternalScoringMessage>>) {
        self.homesending_message_broker
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
            Self::register_event_fn(self.homesending_data_collector.clone()),
            Self::register_partition_fn(
                self.homesending_data_collector.clone(),
                self.homesending_message_broker.clone(),
            ),
            Self::register_mobsim_fn(
                self.homesending_data_collector.clone(),
                self.homesending_message_broker.clone(),
            ),
        )
    }

    fn finish(&self) {
        self.homesending_message_broker
            .lock()
            .unwrap()
            .finish_send_recv();
        let population = self.homesending_data_collector.lock().unwrap().finish();
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

impl HomesendingScoringEngine {
    pub(crate) fn register_event_fn(
        data_collector: Arc<Mutex<HomeSendingDataCollector>>,
    ) -> Box<EventHandlerRegisterFn> {
        Box::new(move |events: &mut EventsManager| {
            // General event forwarding
            let data_collector1 = Arc::clone(&data_collector);
            events.on_any(move |e: &dyn EventTrait| {
                let mut hdc = data_collector1.lock().unwrap();
                hdc.handle_event(e);
            });

            // Events for Vehicle2Person mappings
            let data_collector2 = Arc::clone(&data_collector);
            events.on::<PersonEntersVehicleEvent, _>(move |e: &PersonEntersVehicleEvent| {
                let mut hdc = data_collector2.lock().unwrap();
                hdc.get_vehicles_mut()
                    .entry(e.vehicle.clone())
                    .or_default()
                    .insert(e.person.clone());
            });

            let data_collector3 = Arc::clone(&data_collector);
            events.on::<PersonLeavesVehicleEvent, _>(move |e: &PersonLeavesVehicleEvent| {
                let mut hdc = data_collector3.lock().unwrap();
                hdc.get_vehicles_mut().remove(&e.vehicle);
            });
        })
    }

    pub(crate) fn register_partition_fn(
        data_collector: Arc<Mutex<HomeSendingDataCollector>>,
        message_broker: Arc<Mutex<HomeSendingMessageBroker>>,
    ) -> Box<PartitionListenerRegisterFn> {
        Box::new(move |events| {
            let data_collector1 = Arc::clone(&data_collector);
            let message_broker1 = Arc::clone(&message_broker);

            events.on_event(move |e: &RuntimeEvent<PartitionEvent>| match &e.payload {
                PartitionEvent::VehicleLeavesPartition(i) => {
                    let mut hdc = data_collector1.lock().unwrap();
                    let mut hmb = message_broker1.lock().unwrap();

                    let leaving_vehicle = hdc.remove_leaving_vehicles(&i.vehicle_id);
                    hmb.add_leaving_vehicle(i.to.clone(), i.vehicle_id.clone(), leaving_vehicle);
                }
                PartitionEvent::AgentLeavesPartition(i) => {
                    let hdc = data_collector1.lock().unwrap();
                    let mut hmb = message_broker1.lock().unwrap();

                    // TODO Calling close_block causes a deadlock, therefore the current fix is
                    //      to let the message broker send a message to itself. Try to find a
                    //      cleaner solution.
                    /*
                    if hdc.is_person_at_home(&i.agent_id) {
                        // If this agent is currently in its home partition, there is no need to
                        // send a leave message, as the events are already processed locally.
                        hdc.message_broker.lock().unwrap().close_block(
                            i.agent_id.clone(),
                            hdc.rank,
                            Some(i.clone()),
                        );
                        return;
                    }
                    */

                    let home_partition = hdc.get_persons().get(&i.agent_id).unwrap();
                    hmb.add_leaving_partition_event(
                        *home_partition,
                        i.agent_id.clone(),
                        e.payload.clone(),
                    )
                }
                PartitionEvent::AgentEntersPartition(i) => {
                    let hdc = data_collector1.lock().unwrap();
                    let mut hmb = message_broker1.lock().unwrap();

                    if hdc.is_person_at_home(&i.agent_id) {
                        // If this agent is currently in its home partition, there is no need to
                        // send a leave message, as the events are already processed locally.
                        hmb.open_block(i.agent_id.clone(), *hdc.get_rank());
                        return;
                    }

                    let home_partition = hdc.get_persons().get(&i.agent_id).unwrap();
                    hmb.add_leaving_partition_event(
                        *home_partition,
                        i.agent_id.clone(),
                        e.payload.clone(),
                    )
                }
                PartitionEvent::VehicleEntersPartition(i) => {
                    let mut hdc = data_collector1.lock().unwrap();
                    let mut hmb = message_broker1.lock().unwrap();

                    hdc.get_pending_vehicles_mut().insert(i.vehicle_id.clone());
                    hmb.wait_for_vehicle(i.vehicle_id.clone());
                }
            });
        })
    }

    pub(crate) fn register_mobsim_fn(
        data_collector: Arc<Mutex<HomeSendingDataCollector>>,
        message_broker: Arc<Mutex<HomeSendingMessageBroker>>,
    ) -> Box<MobsimListenerRegisterFn> {
        Box::new(move |events: &mut MobsimEventsManager| {
            let message_broker1 = Arc::clone(&message_broker);
            let data_collector1 = Arc::clone(&data_collector);

            events.on_event(move |e: &RuntimeEvent<MobsimEvent>| match &e.payload {
                MobsimEvent::BeforeSimStep(_) => {
                    message_broker1.lock().unwrap().recv_vehicles();
                    // Broker lock released before replay; handle_event locks the broker internally.
                    data_collector1
                        .lock()
                        .unwrap()
                        .replay_deferred_link_events();
                }
                MobsimEvent::AfterSimStep(_) => {
                    message_broker1.lock().unwrap().send_recv();
                }
                _ => {}
            });
        })
    }
}
