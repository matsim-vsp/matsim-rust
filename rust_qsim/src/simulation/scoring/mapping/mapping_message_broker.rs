use crate::simulation::events::EventTrait;
use crate::simulation::framework_events::{
    MobsimEvent, MobsimEventsManager, MobsimListenerRegisterFn, QSimId, RuntimeEvent,
};
use crate::simulation::id::Id;
use crate::simulation::scenario::population::{InternalPerson, InternalPlan};
use crate::simulation::scenario::vehicles::InternalVehicle;
use crate::simulation::scoring::InternalScoringMessage;
use crate::simulation::scoring::mapping::mapping_data_collector::MappingDataCollector;
use crate::simulation::scoring::mapping::mapping_data_forwarder::MappingDataForwarder;
use ahash::{HashSet, HashSetExt};
use nohash_hasher::IntMap;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex, Weak};

pub struct MappingCollectorMessageBroker {
    receiver: Receiver<InternalScoringMessage>,
    senders: Vec<Sender<InternalScoringMessage>>,
    rank: QSimId,
    num_partitions: usize,
    num_collectors: usize,
    sync_interval: u32,

    counter: u32,
    buffer_events: IntMap<QSimId, IntMap<Id<InternalPerson>, Vec<(Box<dyn EventTrait>, u32)>>>,
    buffer_vehicles: IntMap<u32, IntMap<Id<InternalVehicle>, Vec<(Box<dyn EventTrait>, u32)>>>,
    data_forwarder: Weak<Mutex<MappingDataForwarder>>,
}

impl MappingCollectorMessageBroker {
    pub fn new(
        receiver: Receiver<InternalScoringMessage>,
        senders: Vec<Sender<InternalScoringMessage>>,
        rank: QSimId,
        num_partitions: usize,
        num_collectors: usize,
        sync_interval: u32,
    ) -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(Self {
            receiver,
            senders,
            rank,
            num_partitions,
            num_collectors,
            sync_interval,
            counter: 0,
            buffer_events: IntMap::default(),
            buffer_vehicles: IntMap::default(),
            data_forwarder: Weak::new(),
        }))
    }

    pub(crate) fn attach_senders(&mut self, senders: Vec<Sender<InternalScoringMessage>>) {
        self.senders.extend(senders);
    }

    pub(crate) fn init(
        message_broker: &Arc<Mutex<Self>>,
        data_collector: Weak<Mutex<MappingDataForwarder>>,
    ) {
        message_broker.lock().unwrap().data_forwarder = data_collector;
    }

    pub(crate) fn register_fn(
        scoring_broker: Arc<Mutex<MappingCollectorMessageBroker>>,
    ) -> Box<MobsimListenerRegisterFn> {
        Box::new(move |events: &mut MobsimEventsManager| {
            let broker = Arc::clone(&scoring_broker);
            events.on_event(move |e: &RuntimeEvent<MobsimEvent>| match &e.payload {
                MobsimEvent::AfterSimStep(i) => {
                    broker.lock().unwrap().send(i.time, false);
                }
                _ => {}
            });
        })
    }

    pub(crate) fn add_leaving_person_event(
        &mut self,
        target: u32,
        person_id: Id<InternalPerson>,
        event: Box<dyn EventTrait>,
    ) {
        self.buffer_events
            .entry(target)
            .or_insert_with(|| IntMap::default())
            .entry(person_id)
            .or_insert_with(|| Vec::default())
            .push((event, self.counter));
        self.counter += 1;
    }

    pub(crate) fn add_leaving_vehicle_event(
        &mut self,
        target: u32,
        vehicle_id: Id<InternalVehicle>,
        event: Box<dyn EventTrait>,
    ) {
        self.buffer_vehicles
            .entry(target)
            .or_insert_with(|| IntMap::default())
            .entry(vehicle_id)
            .or_insert_with(|| Vec::default())
            .push((event, self.counter));
        self.counter += 1;
    }

    /// Called on every AfterSimStep: Flushes send buffers
    fn send(&mut self, time: u32, force_sync: bool) {
        for (target, vehicle_events) in self.buffer_vehicles.drain() {
            let msg = InternalScoringMessage {
                from_process: self.rank,
                to_process: target,
                message: Box::new(VehicleEventMessage {
                    events: vehicle_events,
                }),
            };

            self.senders[target as usize].send(msg).unwrap_or_else(|e| {
                panic!(
                    "Error sending VehicleEventMessage to rank {} with error {}",
                    target, e
                );
            });
        }

        for (target, events) in self.buffer_events.drain() {
            let msg = InternalScoringMessage {
                from_process: self.rank,
                to_process: target,
                message: Box::new(PersonEventMessage { events }),
            };

            self.senders[target as usize].send(msg).unwrap_or_else(|e| {
                panic!(
                    "Error sending PersonEventMessage to rank {} with error {}",
                    target, e
                )
            });
        }

        if time % self.sync_interval == 0 || force_sync {
            for target in self.num_partitions..(self.num_partitions + self.num_collectors) {
                let msg = InternalScoringMessage {
                    from_process: self.rank,
                    to_process: target as QSimId,
                    message: Box::new(WatermarkMessage {
                        origin: self.rank,
                        hop: 1,
                        time,
                    }),
                };

                self.senders[target].send(msg).unwrap_or_else(|e| {
                    panic!(
                        "Error sending PersonEventMessage to rank {} with error {}",
                        target, e
                    )
                });
            }
        }
    }

    /// Called after the mobsim ends: Flushes the send buffers and sends a finish message to all scoring threads.
    /// Then collects incoming Experienced Plans and passes them to the forwarder
    pub(crate) fn finish_send_recv(&mut self) {
        self.send(u32::MAX, true);

        // TODO Use finished instead of this for loop
        for _ in 0..self.num_collectors {
            let received_msg = self.receiver.recv().unwrap();

            let boxed_any = received_msg.message.into_any();
            match () {
                _ if boxed_any.is::<InternalPlanMessage>() => {
                    let m = boxed_any.downcast::<InternalPlanMessage>().unwrap();
                    for (person_id, plan) in m.plans {
                        self.data_forwarder
                            .upgrade()
                            .unwrap()
                            .lock()
                            .unwrap()
                            .add_arriving_plan(person_id, plan);
                    }
                }
                _ => {
                    panic!("Received unexpected message type during simulation step!");
                }
            }
        }
    }
}

pub struct MappingScoringMessageBroker {
    receiver: Receiver<InternalScoringMessage>,
    senders: Vec<Sender<InternalScoringMessage>>,
    rank: QSimId,
    num_partitions: usize,
    num_collectors: usize,
    person_id2home_partition: IntMap<Id<InternalPerson>, QSimId>,

    buffer_events: IntMap<u32, IntMap<Id<InternalPerson>, Vec<(Box<dyn EventTrait>, u32)>>>,
    buffer_watermarks: IntMap<QSimId, WatermarkMessage>,
    data_collector: Weak<Mutex<MappingDataCollector>>,
}

impl MappingScoringMessageBroker {
    pub fn new(
        receiver: Receiver<InternalScoringMessage>,
        senders: Vec<Sender<InternalScoringMessage>>,
        rank: QSimId,
        num_partitions: usize,
        num_collectors: usize,
        person_id2home_partition: IntMap<Id<InternalPerson>, QSimId>,
    ) -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(Self {
            receiver,
            senders,
            rank,
            num_partitions,
            num_collectors,
            person_id2home_partition,
            buffer_events: IntMap::default(),
            buffer_watermarks: IntMap::default(),
            data_collector: Weak::new(),
        }))
    }

    pub(crate) fn attach_senders(&mut self, senders: Vec<Sender<InternalScoringMessage>>) {
        self.senders.extend(senders);
    }

    pub(crate) fn init(
        message_broker: &Arc<Mutex<Self>>,
        data_collector: Weak<Mutex<MappingDataCollector>>,
    ) {
        message_broker.lock().unwrap().data_collector = data_collector;
    }

    /// Thread-Function to execute. Consists of blocking recv-send loop, that breaks when all finish messages were received.
    /// Finish procedure consists of sending experienced plans back to the home-partitions.
    pub fn work(&mut self) {
        let mut finished = HashSet::new();
        loop {
            let received_msg = self.receiver.recv().expect("Error receiving message");

            if let Some(partition) = self.recv(received_msg) {
                finished.insert(partition);
            }
            if finished.len() == (self.num_partitions * self.num_collectors) {
                break;
            }

            self.send();
        }

        self.finish_send();
    }

    fn recv(&mut self, msg: InternalScoringMessage) -> Option<(QSimId, QSimId)> {
        let boxed_any = msg.message.into_any();

        match () {
            _ if boxed_any.is::<VehicleEventMessage>() => {
                let m = boxed_any.downcast::<VehicleEventMessage>().unwrap();
                let forwarded_events = self
                    .data_collector
                    .upgrade()
                    .unwrap()
                    .lock()
                    .unwrap()
                    .add_arriving_vehicle_events(m.events);
                self.buffer_events.extend(forwarded_events);
            }
            _ if boxed_any.is::<PersonEventMessage>() => {
                let m = boxed_any.downcast::<PersonEventMessage>().unwrap();
                self.data_collector
                    .upgrade()
                    .unwrap()
                    .lock()
                    .unwrap()
                    .add_arriving_person_events(m.events);
            }
            _ if boxed_any.is::<WatermarkMessage>() => {
                let m = boxed_any.downcast::<WatermarkMessage>().unwrap();

                match m.hop {
                    1 => {
                        for target in
                            self.num_partitions..(self.num_partitions + self.num_collectors)
                        {
                            self.buffer_watermarks.insert(
                                target as QSimId,
                                WatermarkMessage {
                                    origin: msg.from_process,
                                    hop: 2,
                                    time: m.time,
                                },
                            );
                        }
                    }
                    2 => {
                        self.data_collector
                            .upgrade()
                            .unwrap()
                            .lock()
                            .unwrap()
                            .add_arriving_watermark(m.origin, msg.from_process, m.time);

                        if m.time == u32::MAX {
                            return Some((m.origin, msg.from_process));
                        }
                    }
                    _ => panic!("Unexpected amount of hops: {}", m.hop),
                }
            }
            _ => {
                panic!("Received unknown message type!");
            }
        }

        None
    }

    fn send(&mut self) {
        for (target, events) in self.buffer_events.drain() {
            let msg = InternalScoringMessage {
                from_process: self.rank,
                to_process: target,
                message: Box::new(PersonEventMessage { events }),
            };

            self.senders[target as usize].send(msg).unwrap_or_else(|e| {
                panic!(
                    "Error sending VehicleMessage to rank {} with error {}",
                    target, e
                )
            });
        }

        for (target, m) in self.buffer_watermarks.drain() {
            let msg = InternalScoringMessage {
                from_process: self.rank,
                to_process: target,
                message: Box::new(m),
            };

            self.senders[target as usize].send(msg).unwrap_or_else(|e| {
                panic!(
                    "Error sending VehicleMessage to rank {} with error {}",
                    target, e
                )
            });
        }
    }

    fn finish_send(&mut self) {
        // TODO Check if heap is empty
        let mut partition_id2partial_plan: IntMap<
            QSimId,
            IntMap<Id<InternalPerson>, InternalPlan>,
        > = IntMap::default();
        for (person_id, partial_plan) in self
            .data_collector
            .upgrade()
            .unwrap()
            .lock()
            .unwrap()
            .take_person_plans()
        {
            let home_partition = *self.person_id2home_partition.get(&person_id).unwrap();
            partition_id2partial_plan
                .entry(home_partition)
                .or_default()
                .insert(person_id, partial_plan.finish());
        }

        for target in 0..self.num_partitions {
            let msg = InternalScoringMessage {
                from_process: self.rank,
                to_process: target as QSimId,
                message: Box::new(InternalPlanMessage {
                    plans: partition_id2partial_plan
                        .remove(&(target as QSimId))
                        .unwrap_or_default(),
                }),
            };

            self.senders[target].send(msg).unwrap_or_else(|e| {
                panic!(
                    "Error sending VehicleMessage to rank {} with error {}",
                    target, e
                )
            });
        }
    }
}
struct PersonEventMessage {
    events: IntMap<Id<InternalPerson>, Vec<(Box<dyn EventTrait>, u32)>>,
}

struct VehicleEventMessage {
    events: IntMap<Id<InternalVehicle>, Vec<(Box<dyn EventTrait>, u32)>>,
}

struct WatermarkMessage {
    origin: QSimId,
    hop: u32,
    time: u32,
}

struct InternalPlanMessage {
    plans: IntMap<Id<InternalPerson>, InternalPlan>,
}
