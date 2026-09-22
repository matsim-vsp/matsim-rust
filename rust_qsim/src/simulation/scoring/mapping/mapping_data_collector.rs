use crate::simulation::events::{
    EventTrait, LinkEnterEvent, PersonEntersVehicleEvent, PersonLeavesVehicleEvent,
    VehicleEntersTrafficEvent, VehicleLeavesTrafficEvent,
};
use crate::simulation::framework_events::QSimId;
use crate::simulation::id::Id;
use crate::simulation::scenario::population::InternalPerson;
use crate::simulation::scenario::vehicles::InternalVehicle;
use crate::simulation::scoring::mapping::mapping_message_broker::MappingScoringMessageBroker;
use crate::simulation::scoring::partial_plans::PartialPlan;
use ahash::HashSet;
use nohash_hasher::{IntMap, IntSet};
use std::cmp::{Ordering, Reverse};
use std::collections::BinaryHeap;
use std::sync::{Arc, Mutex};

pub struct MappingDataCollector {
    person_hash_function: Box<dyn Fn(Id<InternalPerson>) -> u32 + Send>,
    num_partitions: u32,
    num_collectors: u32,

    person_id2heap: IntMap<Id<InternalPerson>, BinaryHeap<HeapEntry>>,
    person_id2partial_plan: IntMap<Id<InternalPerson>, PartialPlan>,
    vehicle_id2person_ids: IntMap<Id<InternalVehicle>, IntSet<Id<InternalPerson>>>,
    watermark: u32,
    watermark_buffer: IntMap<u32, HashSet<(QSimId, QSimId)>>,

    // TODO: Check for the final version, whether this reference can be really removed
    #[allow(unused)]
    message_broker: Arc<Mutex<MappingScoringMessageBroker>>,
}

impl MappingDataCollector {
    pub fn new(
        person_hash_function: Box<dyn Fn(Id<InternalPerson>) -> u32 + Send>,
        num_partitions: u32,
        num_collectors: u32,
        message_broker: Arc<Mutex<MappingScoringMessageBroker>>,
    ) -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(Self {
            person_hash_function,
            num_partitions,
            num_collectors,
            person_id2heap: IntMap::default(),
            person_id2partial_plan: IntMap::default(),
            vehicle_id2person_ids: IntMap::default(),
            watermark: 0,
            watermark_buffer: IntMap::default(),
            message_broker,
        }))
    }

    pub(crate) fn add_arriving_watermark(&mut self, origin: QSimId, collector: QSimId, time: u32) {
        self.watermark_buffer
            .entry(time)
            .or_default()
            .insert((origin, collector));

        if self.watermark_buffer.get(&time).unwrap().len()
            == (self.num_partitions * self.num_collectors) as usize
        {
            if self.watermark > time {
                panic!(
                    "Broken assertion: Tried to decrease watermark from {} to {} ",
                    self.watermark, time
                );
            }

            self.watermark = time;
            self.watermark_buffer.remove(&time);

            for (person_id, heap) in self.person_id2heap.iter_mut() {
                while let Some(entry) = heap.peek() {
                    if entry.0.0 <= self.watermark {
                        self.person_id2partial_plan
                            .entry(person_id.clone())
                            .or_default()
                            .handle_event(&*heap.pop().unwrap().2);
                    } else {
                        break;
                    }
                }
            }
        }
    }

    pub(crate) fn add_arriving_person_events(
        &mut self,
        arriving_events: IntMap<Id<InternalPerson>, Vec<(Box<dyn EventTrait>, u32)>>,
    ) {
        for (person_id, arriving_events) in arriving_events {
            for (arriving_event, c) in arriving_events {
                self.person_id2heap
                    .entry(person_id.clone())
                    .or_insert_with(BinaryHeap::new)
                    .push(HeapEntry(
                        Reverse(arriving_event.time()),
                        Reverse(c),
                        arriving_event,
                    ));

                // self.person_id2partial_plan.entry(person_id.clone()).or_default().handle_event(&*arriving_event);
            }
        }
    }

    pub(crate) fn add_arriving_vehicle_events(
        &mut self,
        arriving_events: IntMap<Id<InternalVehicle>, Vec<(Box<dyn EventTrait>, u32)>>,
    ) -> IntMap<u32, IntMap<Id<InternalPerson>, Vec<(Box<dyn EventTrait>, u32)>>> {
        let mut buffer_events: IntMap<
            u32,
            IntMap<Id<InternalPerson>, Vec<(Box<dyn EventTrait>, u32)>>,
        > = IntMap::default();

        for (vehicle_id, mut events) in arriving_events {
            for (event, c) in events.drain(..) {
                let event_ref: &dyn EventTrait = &*event;
                if let Some(e) = event_ref.as_any().downcast_ref::<LinkEnterEvent>() {
                    for person_id in self.vehicle_id2person_ids.get(&vehicle_id).unwrap() {
                        buffer_events
                            .entry(
                                (self.person_hash_function)(person_id.clone())
                                    + self.num_partitions,
                            )
                            .or_insert(IntMap::default())
                            .entry(person_id.clone())
                            .or_insert(vec![])
                            .push((Box::new(e.clone()), c));
                    }
                } else if let Some(e) = event_ref
                    .as_any()
                    .downcast_ref::<VehicleEntersTrafficEvent>()
                {
                    for person_id in self.vehicle_id2person_ids.get(&vehicle_id).unwrap() {
                        buffer_events
                            .entry(
                                (self.person_hash_function)(person_id.clone())
                                    + self.num_partitions,
                            )
                            .or_insert(IntMap::default())
                            .entry(person_id.clone())
                            .or_insert(vec![])
                            .push((Box::new(e.clone()), c));
                    }
                } else if let Some(e) = event_ref
                    .as_any()
                    .downcast_ref::<VehicleLeavesTrafficEvent>()
                {
                    for person_id in self.vehicle_id2person_ids.get(&vehicle_id).unwrap() {
                        buffer_events
                            .entry(
                                (self.person_hash_function)(person_id.clone())
                                    + self.num_partitions,
                            )
                            .or_insert(IntMap::default())
                            .entry(person_id.clone())
                            .or_insert(vec![])
                            .push((Box::new(e.clone()), c));
                    }
                } else if let Some(e) = event_ref
                    .as_any()
                    .downcast_ref::<PersonEntersVehicleEvent>()
                {
                    self.vehicle_id2person_ids
                        .entry(e.vehicle.clone())
                        .or_default()
                        .insert(e.person.clone());

                    for person_id in self.vehicle_id2person_ids.get(&vehicle_id).unwrap() {
                        buffer_events
                            .entry(
                                (self.person_hash_function)(person_id.clone())
                                    + self.num_partitions,
                            )
                            .or_insert(IntMap::default())
                            .entry(person_id.clone())
                            .or_insert(vec![])
                            .push((Box::new(e.clone()), c));
                    }
                } else if let Some(e) = event_ref
                    .as_any()
                    .downcast_ref::<PersonLeavesVehicleEvent>()
                {
                    for person_id in self.vehicle_id2person_ids.get(&vehicle_id).unwrap() {
                        buffer_events
                            .entry(
                                (self.person_hash_function)(person_id.clone())
                                    + self.num_partitions,
                            )
                            .or_insert(IntMap::default())
                            .entry(person_id.clone())
                            .or_insert(vec![])
                            .push((Box::new(e.clone()), c));
                    }

                    self.vehicle_id2person_ids
                        .get_mut(&e.vehicle)
                        .unwrap()
                        .remove(&e.person);
                } else {
                    panic!("Unknown event type: '{}'", event_ref.type_())
                }
            }
        }

        buffer_events
    }

    pub(crate) fn take_person_plans(&mut self) -> IntMap<Id<InternalPerson>, PartialPlan> {
        std::mem::take(&mut self.person_id2partial_plan)
    }
}

struct HeapEntry(Reverse<u32>, Reverse<u32>, Box<dyn EventTrait>);

impl Ord for HeapEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.cmp(&other.0).then(self.1.cmp(&other.1))
    }
}
impl PartialOrd for HeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl PartialEq for HeapEntry {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0 && self.1 == other.1
    }
}
impl Eq for HeapEntry {}
