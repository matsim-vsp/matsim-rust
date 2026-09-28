use crate::simulation::events::{
    EventTrait, EventsManager, PersonEntersVehicleEvent, PersonLeavesVehicleEvent,
};
use crate::simulation::framework_events::{MobsimEvent, MobsimEventsManager, QSimId};
use crate::simulation::id::Id;
use crate::simulation::messaging::partition_change::PartitionChangeExtensionsManager;
use crate::simulation::scenario::population::{InternalPerson, Population};
use crate::simulation::scoring::backpacking::backpacking_data_collector::{
    BackpackingAttachment, BackpackingDataCollector,
};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::Sender;

pub(crate) struct BackpackingWorkerResult {
    pub(crate) rank: QSimId,
    pub(crate) iteration: u32,
    pub(crate) population: Population,
}

pub struct BackpackingEngine {
    backpacking_data_collector: BackpackingDataCollector,
}

impl BackpackingEngine {
    pub fn new(home_person_ids: Vec<Id<InternalPerson>>) -> Self {
        Self {
            backpacking_data_collector: BackpackingDataCollector::new(home_person_ids),
        }
    }

    pub(crate) fn register(
        engine: Rc<RefCell<Self>>,
        events: &mut EventsManager,
        mobsim_events: &mut MobsimEventsManager,
        partition_change_extensions: &mut PartitionChangeExtensionsManager,
        result_sender: Sender<BackpackingWorkerResult>,
        rank: QSimId,
    ) {
        Self::register_event_handlers(engine.clone(), events);

        let send_engine = engine.clone();
        let receive_engine = engine.clone();
        partition_change_extensions.register::<BackpackingAttachment, _, _>(
            move |entity, _| {
                Some(
                    send_engine
                        .borrow_mut()
                        .backpacking_data_collector
                        .send(entity),
                )
            },
            move |entity, attachment, _| {
                receive_engine
                    .borrow_mut()
                    .backpacking_data_collector
                    .receive(
                        entity,
                        attachment.expect("Backpacking attachment is missing on arrival."),
                    );
            },
        );

        mobsim_events.on_event(move |event| {
            if event.payload == MobsimEvent::BeforeCleanup {
                let population = engine.borrow_mut().finish();
                result_sender
                    .send(BackpackingWorkerResult {
                        rank,
                        iteration: event.meta.iteration,
                        population,
                    })
                    .unwrap_or_else(|error| {
                        panic!(
                            "Backpacking worker rank {rank} failed to send iteration {} result: {error}",
                            event.meta.iteration
                        )
                    });
            }
        });
    }

    fn register_event_handlers(engine: Rc<RefCell<Self>>, events: &mut EventsManager) {
        let reset_engine = engine.clone();
        events.on_reset_iteration(move |_| {
            reset_engine
                .borrow_mut()
                .backpacking_data_collector
                .reset_iteration();
        });

        let event_engine = engine.clone();
        events.on_any(move |event: &dyn EventTrait| {
            event_engine
                .borrow_mut()
                .backpacking_data_collector
                .handle_event(event);
        });

        let enter_engine = engine.clone();
        events.on::<PersonEntersVehicleEvent, _>(move |event| {
            enter_engine
                .borrow_mut()
                .backpacking_data_collector
                .person_enters_vehicle(event);
        });

        events.on::<PersonLeavesVehicleEvent, _>(move |event| {
            engine
                .borrow_mut()
                .backpacking_data_collector
                .person_leaves_vehicle(event);
        });
    }

    fn finish(&mut self) -> Population {
        self.backpacking_data_collector.finish()
    }
}
