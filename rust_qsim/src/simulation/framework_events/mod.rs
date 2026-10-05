use crate::simulation::events::EventsManager;
use crate::simulation::id::Id;
use crate::simulation::messaging::partition_change::PartitionChangeExtensionsManager;
use crate::simulation::scenario::population::InternalPerson;
use crate::simulation::scenario::vehicles::InternalVehicle;
use crate::simulation::time::SimTime;

pub type QSimId = u32;

pub type MobsimRuntimeEvent = RuntimeEvent<MobsimEvent>;
pub type ControllerRuntimeEvent = RuntimeEvent<ControllerEvent>;
pub type PartitionRuntimeEvent = RuntimeEvent<PartitionEvent>;

pub type MobsimEventsManager = FrameworkEventsManager<MobsimEvent>;
pub type ControllerEventsManager = FrameworkEventsManager<ControllerEvent>;
pub type PartitionEventsManager = FrameworkEventsManager<PartitionEvent>;

pub type ControllerListenerRegisterFn = dyn FnOnce(&mut ControllerEventsManager) + Send;

pub type WorkerListenerRegisterFunction = dyn FnOnce(
        &mut EventsManager,
        &mut MobsimEventsManager,
        &mut PartitionEventsManager,
        &mut PartitionChangeExtensionsManager,
    ) + Send;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PartitionEvent {
    AgentEntersPartition(AgentEntersPartitionEvent),
    AgentLeavesPartition(AgentLeavesPartitionEvent),
    VehicleEntersPartition(VehicleEntersPartitionEvent),
    VehicleLeavesPartition(VehicleLeavesPartitionEvent),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentEntersPartitionEvent {
    pub agent_id: Id<InternalPerson>,
    pub from: QSimId,
    pub time: SimTime,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentLeavesPartitionEvent {
    pub agent_id: Id<InternalPerson>,
    pub to: QSimId,
    pub time: SimTime,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VehicleEntersPartitionEvent {
    pub vehicle_id: Id<InternalVehicle>,
    pub network_mode: Id<String>,
    pub from: QSimId,
    pub time: SimTime,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VehicleLeavesPartitionEvent {
    pub vehicle_id: Id<InternalVehicle>,
    pub to: QSimId,
    pub time: SimTime,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MobsimEvent {
    Initialized,
    BeforeSimStep(MobsimTimeEvent),
    AfterSimStep(MobsimTimeEvent),
    BeforeCleanup,
}

impl MobsimEvent {
    pub fn before_sim_step(time: SimTime) -> Self {
        Self::BeforeSimStep(MobsimTimeEvent { time })
    }

    pub fn after_sim_step(time: SimTime) -> Self {
        Self::AfterSimStep(MobsimTimeEvent { time })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MobsimTimeEvent {
    pub time: SimTime,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ControllerEvent {
    Startup(GeneralControllerEvent),
    IterationStarts(GeneralControllerEvent),
    BeforeMobsim(GeneralControllerEvent),
    AfterMobsim(GeneralControllerEvent),
    Scoring(GeneralControllerEvent),
    Replanning(GeneralControllerEvent),
    IterationEnds(GeneralControllerEvent),
    Shutdown(GeneralControllerEvent),
}

impl ControllerEvent {
    fn with_flag(last_iteration: bool) -> GeneralControllerEvent {
        GeneralControllerEvent { last_iteration }
    }

    pub fn startup(last_iteration: bool) -> Self {
        Self::Startup(Self::with_flag(last_iteration))
    }

    pub fn iteration_starts(last_iteration: bool) -> Self {
        Self::IterationStarts(Self::with_flag(last_iteration))
    }

    pub fn before_mobsim(last_iteration: bool) -> Self {
        Self::BeforeMobsim(Self::with_flag(last_iteration))
    }

    pub fn after_mobsim(last_iteration: bool) -> Self {
        Self::AfterMobsim(Self::with_flag(last_iteration))
    }

    pub fn scoring(last_iteration: bool) -> Self {
        Self::Scoring(Self::with_flag(last_iteration))
    }

    pub fn replanning(last_iteration: bool) -> Self {
        Self::Replanning(Self::with_flag(last_iteration))
    }

    pub fn iteration_ends(last_iteration: bool) -> Self {
        Self::IterationEnds(Self::with_flag(last_iteration))
    }

    pub fn shutdown(last_iteration: bool) -> Self {
        Self::Shutdown(Self::with_flag(last_iteration))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GeneralControllerEvent {
    pub last_iteration: bool,
    // something like "matsim services" in java
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeEvent<E> {
    pub meta: EventMeta,
    pub payload: E,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EventOrigin {
    Controller,
    Partition(QSimId),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventMeta {
    pub origin: EventOrigin,
    pub iteration: u32,
    pub seq_no: u64,
}

#[derive(Debug, Clone, Copy)]
struct EventRuntimeState {
    origin: EventOrigin,
    iteration: u32,
    next_seq_no: u64,
}

impl EventRuntimeState {
    fn new(origin: EventOrigin, iteration: u32) -> Self {
        Self {
            origin,
            iteration,
            next_seq_no: 0,
        }
    }

    fn wrap<E>(&mut self, payload: E) -> RuntimeEvent<E> {
        let event = RuntimeEvent {
            meta: EventMeta {
                origin: self.origin,
                iteration: self.iteration,
                seq_no: self.next_seq_no,
            },
            payload,
        };
        self.next_seq_no += 1;
        event
    }

    fn next_iteration(&mut self) {
        self.reset_iteration(self.iteration + 1);
    }

    fn reset_iteration(&mut self, iteration: u32) {
        self.iteration = iteration;
        self.next_seq_no = 0;
    }
}

type OnRuntimeEventFn<E> = dyn Fn(&RuntimeEvent<E>) + 'static;

struct RegisteredCallback<E> {
    priority: i32,
    callback: Box<OnRuntimeEventFn<E>>,
}

pub struct FrameworkEventsManager<E> {
    state: EventRuntimeState,
    callbacks: Vec<RegisteredCallback<E>>,
}

impl<E> std::fmt::Debug for FrameworkEventsManager<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "FrameworkEventsManager {{ state: {:?}, callbacks: {} }}",
            self.state,
            self.callbacks.len()
        )
    }
}

impl<E> FrameworkEventsManager<E> {
    pub fn new(origin: EventOrigin, iteration: u32) -> Self {
        Self {
            state: EventRuntimeState::new(origin, iteration),
            callbacks: Vec::new(),
        }
    }

    pub fn process_event(&mut self, payload: E) -> RuntimeEvent<E> {
        let event = self.state.wrap(payload);
        for entry in &self.callbacks {
            (entry.callback)(&event);
        }
        event
    }

    pub fn next_iteration(&mut self) {
        self.state.next_iteration();
    }

    pub fn reset_iteration(&mut self, iteration: u32) {
        self.state.reset_iteration(iteration);
    }

    pub fn on_event<F>(&mut self, callback: F)
    where
        F: Fn(&RuntimeEvent<E>) + 'static,
    {
        self.on_event_with_priority(0, callback);
    }

    pub fn on_event_with_priority<F>(&mut self, priority: i32, callback: F)
    where
        F: Fn(&RuntimeEvent<E>) + 'static,
    {
        self.callbacks.push(RegisteredCallback {
            priority,
            callback: Box::new(callback),
        });
        self.callbacks.sort_by(|a, b| b.priority.cmp(&a.priority));
    }
}

impl FrameworkEventsManager<MobsimEvent> {
    pub fn for_partition(qsim_id: QSimId, iteration: u32) -> Self {
        Self::new(EventOrigin::Partition(qsim_id), iteration)
    }
}

#[cfg(test)]
impl Default for FrameworkEventsManager<MobsimEvent> {
    fn default() -> Self {
        Self::for_partition(0, 0)
    }
}

impl FrameworkEventsManager<PartitionEvent> {
    pub fn for_partition(qsim_id: QSimId, iteration: u32) -> Self {
        Self::new(EventOrigin::Partition(qsim_id), iteration)
    }
}

#[cfg(test)]
impl Default for FrameworkEventsManager<PartitionEvent> {
    fn default() -> Self {
        Self::for_partition(0, 0)
    }
}

impl FrameworkEventsManager<ControllerEvent> {
    pub fn for_controller(iteration: u32) -> Self {
        Self::new(EventOrigin::Controller, iteration)
    }
}

impl Default for FrameworkEventsManager<ControllerEvent> {
    fn default() -> Self {
        Self::for_controller(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simulation::time::SimTime;
    use macros::deterministic_id_test;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[test]
    fn controller_events_invoke_callbacks_with_controller_meta() {
        let mut manager = ControllerEventsManager::for_controller(3);
        let received: Rc<RefCell<Vec<ControllerRuntimeEvent>>> = Rc::new(RefCell::new(Vec::new()));
        let received_clone = received.clone();

        manager.on_event(move |event| {
            received_clone.borrow_mut().push(event.clone());
        });

        manager.process_event(ControllerEvent::startup(false));
        manager.process_event(ControllerEvent::iteration_starts(true));

        let events = received.borrow();
        assert_eq!(2, events.len());

        assert_eq!(EventOrigin::Controller, events[0].meta.origin);
        assert_eq!(3, events[0].meta.iteration);
        assert_eq!(0, events[0].meta.seq_no);
        assert_eq!(ControllerEvent::startup(false), events[0].payload.clone());

        assert_eq!(EventOrigin::Controller, events[1].meta.origin);
        assert_eq!(3, events[1].meta.iteration);
        assert_eq!(1, events[1].meta.seq_no);
        assert_eq!(
            ControllerEvent::iteration_starts(true),
            events[1].payload.clone()
        );
    }

    #[test]
    fn controller_next_iteration_resets_seq_counter() {
        let mut manager = ControllerEventsManager::for_controller(10);

        let first = manager.process_event(ControllerEvent::startup(false));
        assert_eq!(10, first.meta.iteration);
        assert_eq!(0, first.meta.seq_no);

        manager.next_iteration();

        let second = manager.process_event(ControllerEvent::shutdown(true));
        assert_eq!(11, second.meta.iteration);
        assert_eq!(0, second.meta.seq_no);
    }

    #[test]
    fn controller_reset_iteration_sets_iteration_and_keeps_callbacks() {
        let mut manager = ControllerEventsManager::for_controller(10);
        let received: Rc<RefCell<Vec<ControllerRuntimeEvent>>> = Rc::new(RefCell::new(Vec::new()));
        let received_clone = received.clone();

        manager.on_event(move |event| {
            received_clone.borrow_mut().push(event.clone());
        });

        manager.process_event(ControllerEvent::startup(false));
        manager.reset_iteration(42);
        let event = manager.process_event(ControllerEvent::shutdown(true));

        assert_eq!(42, event.meta.iteration);
        assert_eq!(0, event.meta.seq_no);
        assert_eq!(2, received.borrow().len());
    }

    #[test]
    fn mobsim_events_use_partition_origin_and_invoke_callbacks() {
        let mut manager = MobsimEventsManager::for_partition(7, 2);
        let received: Rc<RefCell<Vec<MobsimRuntimeEvent>>> = Rc::new(RefCell::new(Vec::new()));
        let received_clone = received.clone();

        manager.on_event(move |event| {
            received_clone.borrow_mut().push(event.clone());
        });

        manager.process_event(MobsimEvent::before_sim_step(SimTime::from_secs(100)));
        manager.process_event(MobsimEvent::after_sim_step(SimTime::from_secs(100)));
        manager.process_event(MobsimEvent::BeforeCleanup);

        let events = received.borrow();
        assert_eq!(3, events.len());

        assert_eq!(EventOrigin::Partition(7), events[0].meta.origin);
        assert_eq!(2, events[0].meta.iteration);
        assert_eq!(0, events[0].meta.seq_no);
        assert_eq!(
            MobsimEvent::before_sim_step(SimTime::from_secs(100)),
            events[0].payload.clone()
        );

        assert_eq!(EventOrigin::Partition(7), events[1].meta.origin);
        assert_eq!(2, events[1].meta.iteration);
        assert_eq!(1, events[1].meta.seq_no);
        assert_eq!(
            MobsimEvent::after_sim_step(SimTime::from_secs(100)),
            events[1].payload.clone()
        );

        assert_eq!(EventOrigin::Partition(7), events[2].meta.origin);
        assert_eq!(2, events[2].meta.iteration);
        assert_eq!(2, events[2].meta.seq_no);
        assert_eq!(MobsimEvent::BeforeCleanup, events[2].payload.clone());
    }

    #[test]
    fn mobsim_next_iteration_resets_seq_counter() {
        let mut manager = MobsimEventsManager::for_partition(1, 5);

        let first = manager.process_event(MobsimEvent::before_sim_step(SimTime::from_secs(1)));
        assert_eq!(5, first.meta.iteration);
        assert_eq!(0, first.meta.seq_no);

        manager.next_iteration();

        let second = manager.process_event(MobsimEvent::after_sim_step(SimTime::from_secs(2)));
        assert_eq!(6, second.meta.iteration);
        assert_eq!(0, second.meta.seq_no);
    }

    #[test]
    fn mobsim_reset_iteration_sets_iteration_and_keeps_callbacks() {
        let mut manager = MobsimEventsManager::for_partition(1, 5);
        let received: Rc<RefCell<Vec<MobsimRuntimeEvent>>> = Rc::new(RefCell::new(Vec::new()));
        let received_clone = received.clone();

        manager.on_event(move |event| {
            received_clone.borrow_mut().push(event.clone());
        });

        manager.process_event(MobsimEvent::before_sim_step(SimTime::from_secs(1)));
        manager.reset_iteration(12);
        let event = manager.process_event(MobsimEvent::after_sim_step(SimTime::from_secs(2)));

        assert_eq!(12, event.meta.iteration);
        assert_eq!(0, event.meta.seq_no);
        assert_eq!(2, received.borrow().len());
    }

    #[test]
    fn controller_callbacks_are_called_by_descending_priority() {
        let mut manager = ControllerEventsManager::for_controller(0);
        let order: Rc<RefCell<Vec<&'static str>>> = Rc::new(RefCell::new(Vec::new()));

        let order_low = order.clone();
        manager.on_event_with_priority(0, move |_| {
            order_low.borrow_mut().push("low");
        });

        let order_high = order.clone();
        manager.on_event_with_priority(10, move |_| {
            order_high.borrow_mut().push("high");
        });

        let order_high_second = order.clone();
        manager.on_event_with_priority(10, move |_| {
            order_high_second.borrow_mut().push("high_2");
        });

        manager.process_event(ControllerEvent::startup(false));

        assert_eq!(vec!["high", "high_2", "low"], order.borrow().clone());
    }

    #[test]
    fn mobsim_callbacks_are_called_by_descending_priority() {
        let mut manager = MobsimEventsManager::for_partition(2, 0);
        let order: Rc<RefCell<Vec<&'static str>>> = Rc::new(RefCell::new(Vec::new()));

        let order_default = order.clone();
        manager.on_event(move |_| {
            order_default.borrow_mut().push("default");
        });

        let order_high = order.clone();
        manager.on_event_with_priority(5, move |_| {
            order_high.borrow_mut().push("high");
        });

        let order_low = order.clone();
        manager.on_event_with_priority(-1, move |_| {
            order_low.borrow_mut().push("low");
        });

        manager.process_event(MobsimEvent::before_sim_step(SimTime::from_secs(10)));

        assert_eq!(vec!["high", "default", "low"], order.borrow().clone());
    }

    #[deterministic_id_test]
    fn partition_events_use_partition_origin_and_invoke_callbacks() {
        let mut manager = PartitionEventsManager::for_partition(4, 6);
        let received: Rc<RefCell<Vec<PartitionRuntimeEvent>>> = Rc::new(RefCell::new(Vec::new()));
        let received_clone = received.clone();

        manager.on_event(move |event| {
            received_clone.borrow_mut().push(event.clone());
        });

        manager.process_event(PartitionEvent::VehicleLeavesPartition(
            VehicleLeavesPartitionEvent {
                vehicle_id: Id::create("veh-1"),
                to: 9,
                time: SimTime::from_secs(42),
            },
        ));
        manager.process_event(PartitionEvent::AgentEntersPartition(
            AgentEntersPartitionEvent {
                agent_id: Id::create("agent-1"),
                from: 2,
                time: SimTime::from_secs(43),
            },
        ));

        let events = received.borrow();
        assert_eq!(2, events.len());

        assert_eq!(EventOrigin::Partition(4), events[0].meta.origin);
        assert_eq!(6, events[0].meta.iteration);
        assert_eq!(0, events[0].meta.seq_no);
        assert_eq!(
            PartitionEvent::VehicleLeavesPartition(VehicleLeavesPartitionEvent {
                vehicle_id: Id::create("veh-1"),
                to: 9,
                time: SimTime::from_secs(42),
            }),
            events[0].payload.clone()
        );

        assert_eq!(EventOrigin::Partition(4), events[1].meta.origin);
        assert_eq!(6, events[1].meta.iteration);
        assert_eq!(1, events[1].meta.seq_no);
        assert_eq!(
            PartitionEvent::AgentEntersPartition(AgentEntersPartitionEvent {
                agent_id: Id::create("agent-1"),
                from: 2,
                time: SimTime::from_secs(43),
            }),
            events[1].payload.clone()
        );
    }

    #[deterministic_id_test]
    fn partition_next_iteration_resets_seq_counter() {
        let mut manager = PartitionEventsManager::for_partition(3, 8);

        let first = manager.process_event(PartitionEvent::VehicleEntersPartition(
            VehicleEntersPartitionEvent {
                vehicle_id: Id::create("veh-2"),
                network_mode: Id::create("car"),
                from: 6,
                time: SimTime::from_secs(10),
            },
        ));
        assert_eq!(8, first.meta.iteration);
        assert_eq!(0, first.meta.seq_no);

        manager.next_iteration();

        let second = manager.process_event(PartitionEvent::AgentLeavesPartition(
            AgentLeavesPartitionEvent {
                agent_id: Id::create("agent-2"),
                to: 7,
                time: SimTime::from_secs(20),
            },
        ));
        assert_eq!(9, second.meta.iteration);
        assert_eq!(0, second.meta.seq_no);
    }

    #[deterministic_id_test]
    fn partition_reset_iteration_sets_iteration_and_keeps_callbacks() {
        let mut manager = PartitionEventsManager::for_partition(3, 8);
        let received: Rc<RefCell<Vec<PartitionRuntimeEvent>>> = Rc::new(RefCell::new(Vec::new()));
        let received_clone = received.clone();

        manager.on_event(move |event| {
            received_clone.borrow_mut().push(event.clone());
        });

        manager.process_event(PartitionEvent::VehicleEntersPartition(
            VehicleEntersPartitionEvent {
                vehicle_id: Id::create("veh-2"),
                network_mode: Id::create("car"),
                from: 6,
                time: SimTime::from_secs(10),
            },
        ));
        manager.reset_iteration(13);
        let event = manager.process_event(PartitionEvent::AgentLeavesPartition(
            AgentLeavesPartitionEvent {
                agent_id: Id::create("agent-2"),
                to: 7,
                time: SimTime::from_secs(20),
            },
        ));

        assert_eq!(13, event.meta.iteration);
        assert_eq!(0, event.meta.seq_no);
        assert_eq!(2, received.borrow().len());
    }

    #[deterministic_id_test]
    fn partition_callbacks_are_called_by_descending_priority() {
        let mut manager = PartitionEventsManager::for_partition(5, 0);
        let order: Rc<RefCell<Vec<&'static str>>> = Rc::new(RefCell::new(Vec::new()));

        let order_default = order.clone();
        manager.on_event(move |_| {
            order_default.borrow_mut().push("default");
        });

        let order_high = order.clone();
        manager.on_event_with_priority(7, move |_| {
            order_high.borrow_mut().push("high");
        });

        let order_low = order.clone();
        manager.on_event_with_priority(-2, move |_| {
            order_low.borrow_mut().push("low");
        });

        manager.process_event(PartitionEvent::VehicleEntersPartition(
            VehicleEntersPartitionEvent {
                vehicle_id: Id::create("veh-3"),
                network_mode: Id::create("car"),
                from: 1,
                time: SimTime::from_secs(5),
            },
        ));

        assert_eq!(vec!["high", "default", "low"], order.borrow().clone());
    }
}
