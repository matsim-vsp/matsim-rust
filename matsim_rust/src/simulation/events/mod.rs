mod comparison;
pub mod utils;

use crate::simulation::InternalAttributes;
use crate::simulation::id::{CreateMissingIds, Id, IdResolver};
use crate::simulation::scenario::Coordinate;
use crate::simulation::scenario::network::Link;
use crate::simulation::scenario::population::InternalPerson;
use crate::simulation::scenario::vehicles::InternalVehicle;
use crate::simulation::time::SimTime;
use macros::event_struct;
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::fmt::{Debug, Formatter};
use std::rc::Rc;

pub trait DynEq: Any {
    fn as_any(&self) -> &dyn Any;
    fn dyn_eq(&self, other: &dyn DynEq) -> bool;
}

pub trait EventTrait: Debug + DynEq + Send + Sync {
    //This can't be a const, because traits with const fields are not dyn compatible.
    fn type_(&self) -> &'static str;
    // fn as_any(&self) -> &dyn Any;
    fn time(&self) -> SimTime;
    fn attributes(&self) -> &InternalAttributes;
}

/// Trait for objects that need to be compared, but whose type is not known at compile time. This is
/// needed for comparing event files, since it is not known which type of event will be read from
/// the files.
/// Based on https://users.rust-lang.org/t/how-to-compare-two-trait-objects-for-equality/88063/5,
/// or specifically, the demo crate https://crates.io/crates/dyn_ord, written by the forum user who
/// wrote the reply in the link above.
/// Main idea: when comparing a and b which are both of type &dyn DynEq, try to downcast b to the
/// type of a. If that works, compare them with the normal equality operator. If not, return false,
/// since they are of different types and thus not equal.
impl<T: Any + PartialEq> DynEq for T {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn dyn_eq(&self, other: &dyn DynEq) -> bool {
        if let Some(other) = other.as_any().downcast_ref::<T>() {
            *self == *other
        } else {
            false
        }
    }
}

impl PartialEq for dyn EventTrait {
    fn eq(&self, other: &Self) -> bool {
        self.dyn_eq(other)
    }
}

type HandleEventFn = dyn Fn(&dyn EventTrait) + 'static;
type ResetIterationFn = dyn Fn(u32) + 'static;
type FinishFn = dyn Fn() + 'static;

/// This is a meta function. It is used to register functions at the [EventsManager] that handle events. Also check the documentation there.
/// This function gets a `&mut` to [EventsManager] and then registers the callbacks for the specific event types.
/// This mechanism allows
/// ```
/// use matsim_rust::simulation::events::{EventTrait, EventsManager, LinkEnterEvent};
/// let f = |events: &mut EventsManager| {
///     events.on_any(|ev: &dyn EventTrait| println!("{:?}", ev));
///     events.on::<LinkEnterEvent, _>(|le: &LinkEnterEvent| println!("This is a LinkEnterEvent: {:?}", le));
/// };
/// ```
pub type EventHandlerRegisterFn = dyn FnOnce(&mut EventsManager) + Send;

/// The EventsManager holds call-backs for event processing. This might seem a bit odd
/// (in particular in comparison to the Java implementation). The reason is that Rust has no reflection, and this
/// architecture allows compile-time checking of the event types.
///
/// There are two ways to register event handlers: (1) `on` and (2) `on_any`. For (1), you need to specify the event type. For (2), you don't.
/// Note, that the impact of `on_any` registrations compared with `on` registrations is much higher on the runtime,
/// since `on_any` callbacks are called for every event, regardless of it is needed or not. The callback needs to decide whether it wants to handle the event or not.
#[derive(Default)]
pub struct EventsManager {
    per_type: HashMap<TypeId, Vec<Rc<HandleEventFn>>>,
    catch_all: Vec<Box<HandleEventFn>>,
    reset_iteration: Vec<Box<ResetIterationFn>>,
    finish: Vec<Box<FinishFn>>,
}

impl Debug for EventsManager {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "EventsManager {{ per_type: {:?}, catch_all: {:?}, reset_iteration: {:?}, finish: {:?} }}",
            self.per_type.len(),
            self.catch_all.len(),
            self.reset_iteration.len(),
            self.finish.len()
        )
    }
}

impl EventsManager {
    pub fn new() -> Self {
        EventsManager {
            per_type: HashMap::new(),
            catch_all: Vec::new(),
            reset_iteration: Vec::new(),
            finish: Vec::new(),
        }
    }

    pub fn process_event(&mut self, event: &dyn EventTrait) {
        let tid = event.as_any().type_id();
        if let Some(list) = self.per_type.get(&tid).cloned() {
            for h in list {
                h(event);
            }
        }
        for h in &self.catch_all {
            h(event);
        }
    }

    pub fn finish(&mut self) {
        for f in self.finish.iter_mut() {
            f()
        }
    }

    pub fn reset_iteration(&mut self, iteration: u32) {
        for f in &self.reset_iteration {
            f(iteration);
        }
    }

    /// This function is used to register callbacks for specific event types.
    pub fn on<E, F>(&mut self, f: F)
    where
        E: EventTrait,
        F: Fn(&E) + 'static,
    {
        let type_id = TypeId::of::<E>();
        let entry = self.per_type.entry(type_id).or_default();
        entry.push(Rc::new(move |ev: &dyn EventTrait| {
            if let Some(e) = ev.as_any().downcast_ref::<E>() {
                f(e);
            }
        }));
    }

    /// This function is used to register callbacks for all event types.
    pub fn on_any<F>(&mut self, f: F)
    where
        F: Fn(&dyn EventTrait) + 'static,
    {
        self.catch_all.push(Box::new(f));
    }

    pub fn on_finish<F>(&mut self, f: F)
    where
        F: Fn() + 'static,
    {
        self.finish.push(Box::new(f));
    }

    pub fn on_reset_iteration<F>(&mut self, f: F)
    where
        F: Fn(u32) + 'static,
    {
        self.reset_iteration.push(Box::new(f));
    }
}

/// Events takes a writer. This is the trait for that
pub trait EventsWriter: EventsWriterClone + Send {
    fn write(&self, buf: Vec<u8>);
}

/// Since we want Events to be clonable but we also want the writer to be a dynamic object, we need to
/// specify how to clone the Box<dyn EventsWriter> property in Events. The following three method are
/// concerned about that. Implemented according to https://stackoverflow.com/questions/50017987/cant-clone-vecboxtrait-because-trait-cannot-be-made-into-an-object
///
pub trait EventsWriterClone {
    fn clone_boxed(&self) -> Box<dyn EventsWriter>;
}

/// this implements clone_boxed for all owned EventsWriters e.g. Box<dyn EventsWriter>. The 'static
/// lifetime applies to all values that are owned. See last paragraph (Trait bound) of
/// https://doc.rust-lang.org/rust-by-example/scope/lifetime/static_lifetime.html
impl<T: EventsWriter + Clone + 'static> EventsWriterClone for T {
    fn clone_boxed(&self) -> Box<dyn EventsWriter> {
        Box::new(self.clone())
    }
}

impl Clone for Box<dyn EventsWriter> {
    fn clone(&self) -> Self {
        self.clone_boxed()
    }
}

#[event_struct]
pub struct GenericEvent {
    pub time: SimTime,
    #[builder(default)]
    pub attributes: InternalAttributes,
}

impl GenericEvent {
    pub const TYPE: &'static str = "generic";
    pub fn from_proto_event(event: &crate::generated::events::GenericEvent, time: SimTime) -> Self {
        Self::try_from_proto_event(event, time, &CreateMissingIds)
            .expect("Creating missing ids never fails.")
    }

    /// Like [`Self::from_proto_event`], but returns `None` if `ids` doesn't resolve an id.
    pub(crate) fn try_from_proto_event(
        event: &crate::generated::events::GenericEvent,
        time: SimTime,
        _ids: &impl IdResolver,
    ) -> Option<Self> {
        let attrs = InternalAttributes::from(&event.attributes);
        assert!(event.r#type.eq(Self::TYPE));
        Some(
            GenericEventBuilder::default()
                .time(time)
                .attributes(attrs)
                .build()
                .unwrap(),
        )
    }
}

#[event_struct]
pub struct ActivityStartEvent {
    pub time: SimTime,
    pub person: Id<InternalPerson>,
    pub link: Id<Link>,
    pub coordinate: Coordinate,
    pub act_type: Id<String>,
    #[builder(default)]
    pub attributes: InternalAttributes,
}

impl ActivityStartEvent {
    pub const TYPE: &'static str = "actstart";
    pub fn from_proto_event(event: &crate::generated::events::GenericEvent, time: SimTime) -> Self {
        Self::try_from_proto_event(event, time, &CreateMissingIds)
            .expect("Creating missing ids never fails.")
    }

    /// Like [`Self::from_proto_event`], but returns `None` if `ids` doesn't resolve an id.
    pub(crate) fn try_from_proto_event(
        event: &crate::generated::events::GenericEvent,
        time: SimTime,
        ids: &impl IdResolver,
    ) -> Option<Self> {
        let attrs = InternalAttributes::from(&event.attributes);
        assert!(event.r#type.eq(Self::TYPE));
        Some(
            ActivityStartEventBuilder::default()
                .time(time)
                .person(ids.resolve(&event.attributes["person"].as_string())?)
                .link(ids.resolve(&event.attributes["link"].as_string())?)
                .act_type(ids.resolve(&event.attributes["act_type"].as_string())?)
                .coordinate(Coordinate::new_3d(
                    event.attributes["x"].as_double(),
                    event.attributes["y"].as_double(),
                    event.attributes["z"].as_double_opt().unwrap_or_default(),
                ))
                .attributes(attrs)
                .build()
                .unwrap(),
        )
    }
}

#[event_struct]
pub struct ActivityEndEvent {
    pub time: SimTime,
    pub person: Id<InternalPerson>,
    pub link: Id<Link>,
    pub coordinate: Coordinate,
    pub act_type: Id<String>,
    #[builder(default)]
    pub attributes: InternalAttributes,
}

impl ActivityEndEvent {
    pub const TYPE: &'static str = "actend";
    pub fn from_proto_event(event: &crate::generated::events::GenericEvent, time: SimTime) -> Self {
        Self::try_from_proto_event(event, time, &CreateMissingIds)
            .expect("Creating missing ids never fails.")
    }

    /// Like [`Self::from_proto_event`], but returns `None` if `ids` doesn't resolve an id.
    pub(crate) fn try_from_proto_event(
        event: &crate::generated::events::GenericEvent,
        time: SimTime,
        ids: &impl IdResolver,
    ) -> Option<Self> {
        let attrs = InternalAttributes::from(&event.attributes);
        assert!(event.r#type.eq(Self::TYPE));
        Some(
            ActivityEndEventBuilder::default()
                .time(time)
                .person(ids.resolve(&event.attributes["person"].as_string())?)
                .link(ids.resolve(&event.attributes["link"].as_string())?)
                .act_type(ids.resolve(&event.attributes["act_type"].as_string())?)
                .coordinate(Coordinate::new_3d(
                    event.attributes["x"].as_double(),
                    event.attributes["y"].as_double(),
                    event.attributes["z"].as_double_opt().unwrap_or_default(),
                ))
                .attributes(attrs)
                .build()
                .unwrap(),
        )
    }
}

#[event_struct]
pub struct LinkEnterEvent {
    pub time: SimTime,
    pub link: Id<Link>,
    pub vehicle: Id<InternalVehicle>,
    #[builder(default)]
    pub attributes: InternalAttributes,
}

impl LinkEnterEvent {
    pub const TYPE: &'static str = "entered link";
    pub fn from_proto_event(event: &crate::generated::events::GenericEvent, time: SimTime) -> Self {
        Self::try_from_proto_event(event, time, &CreateMissingIds)
            .expect("Creating missing ids never fails.")
    }

    /// Like [`Self::from_proto_event`], but returns `None` if `ids` doesn't resolve an id.
    pub(crate) fn try_from_proto_event(
        event: &crate::generated::events::GenericEvent,
        time: SimTime,
        ids: &impl IdResolver,
    ) -> Option<Self> {
        let attrs = InternalAttributes::from(&event.attributes);
        assert!(event.r#type.eq(Self::TYPE));
        Some(
            LinkEnterEventBuilder::default()
                .time(time)
                .link(ids.resolve(&event.attributes["link"].as_string())?)
                .vehicle(ids.resolve(&event.attributes["vehicle"].as_string())?)
                .attributes(attrs)
                .build()
                .unwrap(),
        )
    }
}

#[event_struct]
pub struct LinkLeaveEvent {
    pub time: SimTime,
    pub link: Id<Link>,
    pub vehicle: Id<InternalVehicle>,
    #[builder(default)]
    pub attributes: InternalAttributes,
}

impl LinkLeaveEvent {
    pub const TYPE: &'static str = "left link";
    pub fn from_proto_event(event: &crate::generated::events::GenericEvent, time: SimTime) -> Self {
        Self::try_from_proto_event(event, time, &CreateMissingIds)
            .expect("Creating missing ids never fails.")
    }

    /// Like [`Self::from_proto_event`], but returns `None` if `ids` doesn't resolve an id.
    pub(crate) fn try_from_proto_event(
        event: &crate::generated::events::GenericEvent,
        time: SimTime,
        ids: &impl IdResolver,
    ) -> Option<Self> {
        let attrs = InternalAttributes::from(&event.attributes);
        assert!(event.r#type.eq(Self::TYPE));
        Some(
            LinkLeaveEventBuilder::default()
                .time(time)
                .link(ids.resolve(&event.attributes["link"].as_string())?)
                .vehicle(ids.resolve(&event.attributes["vehicle"].as_string())?)
                .attributes(attrs)
                .build()
                .unwrap(),
        )
    }
}

#[event_struct]
pub struct VehicleEntersTrafficEvent {
    pub time: SimTime,
    pub vehicle: Id<InternalVehicle>,
    pub link: Id<Link>,
    pub person: Id<InternalPerson>,
    pub network_mode: Id<String>,
    #[builder(default = 1.0)]
    pub relative_position: f64,
    #[builder(default)]
    pub attributes: InternalAttributes,
}

impl VehicleEntersTrafficEvent {
    pub const TYPE: &'static str = "vehicle enters traffic";
    pub fn from_proto_event(event: &crate::generated::events::GenericEvent, time: SimTime) -> Self {
        Self::try_from_proto_event(event, time, &CreateMissingIds)
            .expect("Creating missing ids never fails.")
    }

    /// Like [`Self::from_proto_event`], but returns `None` if `ids` doesn't resolve an id.
    pub(crate) fn try_from_proto_event(
        event: &crate::generated::events::GenericEvent,
        time: SimTime,
        ids: &impl IdResolver,
    ) -> Option<Self> {
        let attrs = InternalAttributes::from(&event.attributes);
        assert!(event.r#type.eq(Self::TYPE));
        Some(
            VehicleEntersTrafficEventBuilder::default()
                .time(time)
                .vehicle(ids.resolve(&event.attributes["vehicle"].as_string())?)
                .link(ids.resolve(&event.attributes["link"].as_string())?)
                .person(ids.resolve(&event.attributes["person"].as_string())?)
                .network_mode(ids.resolve(&event.attributes["network_mode"].as_string())?)
                .relative_position(event.attributes["relative_position"].as_double())
                .attributes(attrs)
                .build()
                .unwrap(),
        )
    }
}

#[event_struct]
pub struct VehicleLeavesTrafficEvent {
    pub time: SimTime,
    pub vehicle: Id<InternalVehicle>,
    pub link: Id<Link>,
    pub person: Id<InternalPerson>,
    pub network_mode: Id<String>,
    #[builder(default = 1.0)]
    pub relative_position: f64,
    #[builder(default)]
    pub attributes: InternalAttributes,
}

impl VehicleLeavesTrafficEvent {
    pub const TYPE: &'static str = "vehicle leaves traffic";
    pub fn from_proto_event(event: &crate::generated::events::GenericEvent, time: SimTime) -> Self {
        Self::try_from_proto_event(event, time, &CreateMissingIds)
            .expect("Creating missing ids never fails.")
    }

    /// Like [`Self::from_proto_event`], but returns `None` if `ids` doesn't resolve an id.
    pub(crate) fn try_from_proto_event(
        event: &crate::generated::events::GenericEvent,
        time: SimTime,
        ids: &impl IdResolver,
    ) -> Option<Self> {
        let attrs = InternalAttributes::from(&event.attributes);
        assert!(event.r#type.eq(Self::TYPE));
        Some(
            VehicleLeavesTrafficEventBuilder::default()
                .time(time)
                .vehicle(ids.resolve(&event.attributes["vehicle"].as_string())?)
                .link(ids.resolve(&event.attributes["link"].as_string())?)
                .person(ids.resolve(&event.attributes["person"].as_string())?)
                .network_mode(ids.resolve(&event.attributes["network_mode"].as_string())?)
                .relative_position(event.attributes["relative_position"].as_double())
                .attributes(attrs)
                .build()
                .unwrap(),
        )
    }
}

#[event_struct]
pub struct PersonEntersVehicleEvent {
    pub time: SimTime,
    pub person: Id<InternalPerson>,
    pub vehicle: Id<InternalVehicle>,
    #[builder(default)]
    pub attributes: InternalAttributes,
}

impl PersonEntersVehicleEvent {
    pub const TYPE: &'static str = "PersonEntersVehicle";
    pub fn from_proto_event(event: &crate::generated::events::GenericEvent, time: SimTime) -> Self {
        Self::try_from_proto_event(event, time, &CreateMissingIds)
            .expect("Creating missing ids never fails.")
    }

    /// Like [`Self::from_proto_event`], but returns `None` if `ids` doesn't resolve an id.
    pub(crate) fn try_from_proto_event(
        event: &crate::generated::events::GenericEvent,
        time: SimTime,
        ids: &impl IdResolver,
    ) -> Option<Self> {
        let attrs = InternalAttributes::from(&event.attributes);
        assert!(event.r#type.eq(Self::TYPE));
        Some(
            PersonEntersVehicleEventBuilder::default()
                .time(time)
                .person(ids.resolve(&event.attributes["person"].as_string())?)
                .vehicle(ids.resolve(&event.attributes["vehicle"].as_string())?)
                .attributes(attrs)
                .build()
                .unwrap(),
        )
    }
}

#[event_struct]
pub struct PersonLeavesVehicleEvent {
    pub time: SimTime,
    pub person: Id<InternalPerson>,
    pub vehicle: Id<InternalVehicle>,
    #[builder(default)]
    pub attributes: InternalAttributes,
}

impl PersonLeavesVehicleEvent {
    pub const TYPE: &'static str = "PersonLeavesVehicle";
    pub fn from_proto_event(event: &crate::generated::events::GenericEvent, time: SimTime) -> Self {
        Self::try_from_proto_event(event, time, &CreateMissingIds)
            .expect("Creating missing ids never fails.")
    }

    /// Like [`Self::from_proto_event`], but returns `None` if `ids` doesn't resolve an id.
    pub(crate) fn try_from_proto_event(
        event: &crate::generated::events::GenericEvent,
        time: SimTime,
        ids: &impl IdResolver,
    ) -> Option<Self> {
        let attrs = InternalAttributes::from(&event.attributes);
        assert!(event.r#type.eq(Self::TYPE));
        Some(
            PersonLeavesVehicleEventBuilder::default()
                .time(time)
                .person(ids.resolve(&event.attributes["person"].as_string())?)
                .vehicle(ids.resolve(&event.attributes["vehicle"].as_string())?)
                .attributes(attrs)
                .build()
                .unwrap(),
        )
    }
}

#[event_struct]
pub struct PersonDepartureEvent {
    pub time: SimTime,
    pub person: Id<InternalPerson>,
    pub link: Id<Link>,
    pub leg_mode: Id<String>,
    pub routing_mode: Id<String>,
    #[builder(default)]
    pub attributes: InternalAttributes,
}

impl PersonDepartureEvent {
    pub const TYPE: &'static str = "departure";
    pub fn from_proto_event(event: &crate::generated::events::GenericEvent, time: SimTime) -> Self {
        Self::try_from_proto_event(event, time, &CreateMissingIds)
            .expect("Creating missing ids never fails.")
    }

    /// Like [`Self::from_proto_event`], but returns `None` if `ids` doesn't resolve an id.
    pub(crate) fn try_from_proto_event(
        event: &crate::generated::events::GenericEvent,
        time: SimTime,
        ids: &impl IdResolver,
    ) -> Option<Self> {
        let attrs = InternalAttributes::from(&event.attributes);
        assert!(event.r#type.eq(Self::TYPE));
        Some(
            PersonDepartureEventBuilder::default()
                .time(time)
                .person(ids.resolve(&event.attributes["person"].as_string())?)
                .link(ids.resolve(&event.attributes["link"].as_string())?)
                .leg_mode(ids.resolve(&event.attributes["mode"].as_string())?)
                .routing_mode(ids.resolve(&event.attributes["routing_mode"].as_string())?)
                .attributes(attrs)
                .build()
                .unwrap(),
        )
    }
}

#[event_struct]
pub struct PersonArrivalEvent {
    pub time: SimTime,
    pub person: Id<InternalPerson>,
    pub link: Id<Link>,
    pub leg_mode: Id<String>,
    #[builder(default)]
    pub attributes: InternalAttributes,
}

impl PersonArrivalEvent {
    pub const TYPE: &'static str = "arrival";
    pub fn from_proto_event(event: &crate::generated::events::GenericEvent, time: SimTime) -> Self {
        Self::try_from_proto_event(event, time, &CreateMissingIds)
            .expect("Creating missing ids never fails.")
    }

    /// Like [`Self::from_proto_event`], but returns `None` if `ids` doesn't resolve an id.
    pub(crate) fn try_from_proto_event(
        event: &crate::generated::events::GenericEvent,
        time: SimTime,
        ids: &impl IdResolver,
    ) -> Option<Self> {
        let attrs = InternalAttributes::from(&event.attributes);
        assert!(event.r#type.eq(Self::TYPE));
        Some(
            PersonArrivalEventBuilder::default()
                .time(time)
                .person(ids.resolve(&event.attributes["person"].as_string())?)
                .link(ids.resolve(&event.attributes["link"].as_string())?)
                .leg_mode(ids.resolve(&event.attributes["mode"].as_string())?)
                .attributes(attrs)
                .build()
                .unwrap(),
        )
    }
}

#[event_struct]
pub struct TeleportationArrivalEvent {
    pub time: SimTime,
    pub person: Id<InternalPerson>,
    pub mode: Id<String>,
    pub distance: f64,
    #[builder(default)]
    pub attributes: InternalAttributes,
}

impl TeleportationArrivalEvent {
    pub const TYPE: &'static str = "travelled";
    pub fn from_proto_event(event: &crate::generated::events::GenericEvent, time: SimTime) -> Self {
        Self::try_from_proto_event(event, time, &CreateMissingIds)
            .expect("Creating missing ids never fails.")
    }

    /// Like [`Self::from_proto_event`], but returns `None` if `ids` doesn't resolve an id.
    pub(crate) fn try_from_proto_event(
        event: &crate::generated::events::GenericEvent,
        time: SimTime,
        ids: &impl IdResolver,
    ) -> Option<Self> {
        let attrs = InternalAttributes::from(&event.attributes);
        assert!(event.r#type.eq(Self::TYPE));
        Some(
            TeleportationArrivalEventBuilder::default()
                .time(time)
                .person(ids.resolve(&event.attributes["person"].as_string())?)
                .mode(ids.resolve(&event.attributes["mode"].as_string())?)
                .distance(event.attributes["distance"].as_string().parse().unwrap())
                .attributes(attrs)
                .build()
                .unwrap(),
        )
    }
}

#[event_struct]
pub struct PtTeleportationArrivalEvent {
    pub time: SimTime,
    pub person: Id<InternalPerson>,
    pub distance: f64,
    pub mode: Id<String>,
    pub route: Id<String>,
    pub line: Id<String>,
    pub boarding_time: SimTime,
    pub access_facility: Id<String>,
    pub egress_facility: Id<String>,
    #[builder(default)]
    pub attributes: InternalAttributes,
}

impl PtTeleportationArrivalEvent {
    pub const TYPE: &'static str = "travelled with pt";
    pub fn from_proto_event(event: &crate::generated::events::GenericEvent, time: SimTime) -> Self {
        Self::try_from_proto_event(event, time, &CreateMissingIds)
            .expect("Creating missing ids never fails.")
    }

    /// Like [`Self::from_proto_event`], but returns `None` if `ids` doesn't resolve an id.
    pub(crate) fn try_from_proto_event(
        event: &crate::generated::events::GenericEvent,
        time: SimTime,
        ids: &impl IdResolver,
    ) -> Option<Self> {
        let attrs = InternalAttributes::from(&event.attributes);
        assert!(event.r#type.eq(Self::TYPE));
        Some(
            PtTeleportationArrivalEventBuilder::default()
                .time(time)
                .person(ids.resolve(&event.attributes["person"].as_string())?)
                .distance(event.attributes["distance"].as_string().parse().unwrap())
                .mode(ids.resolve(&event.attributes["mode"].as_string())?)
                .route(ids.resolve(&event.attributes["route"].as_string())?)
                .line(ids.resolve(&event.attributes["line"].as_string())?)
                .boarding_time(SimTime::from_nanos(
                    event.attributes["boardingTimeNs"]
                        .as_string()
                        .parse()
                        .unwrap(),
                ))
                .access_facility(ids.resolve(&event.attributes["accessFacility"].as_string())?)
                .egress_facility(ids.resolve(&event.attributes["egressFacility"].as_string())?)
                .attributes(attrs)
                .build()
                .unwrap(),
        )
    }
}

#[event_struct]
pub struct PersonStuckEvent {
    pub time: SimTime,
    pub person: Id<InternalPerson>,
    #[builder(default)]
    pub link: Option<Id<Link>>,
    #[builder(default)]
    pub leg_mode: Option<Id<String>>,
    #[builder(default)]
    pub reason: Option<String>,
    #[builder(default)]
    pub attributes: InternalAttributes,
}

impl PersonStuckEvent {
    pub const TYPE: &'static str = "stuckAndAbort";
    pub fn from_proto_event(event: &crate::generated::events::GenericEvent, time: SimTime) -> Self {
        Self::try_from_proto_event(event, time, &CreateMissingIds)
            .expect("Creating missing ids never fails.")
    }

    /// Like [`Self::from_proto_event`], but returns `None` if `ids` doesn't resolve an id.
    pub(crate) fn try_from_proto_event(
        event: &crate::generated::events::GenericEvent,
        time: SimTime,
        ids: &impl IdResolver,
    ) -> Option<Self> {
        let attrs = InternalAttributes::from(&event.attributes);
        assert!(event.r#type.eq(Self::TYPE));
        Some(
            PersonStuckEventBuilder::default()
                .time(time)
                .person(ids.resolve(&event.attributes["person"].as_string())?)
                .leg_mode(match event.attributes.get("leg_mode") {
                    Some(value) => Some(ids.resolve(&value.as_string())?),
                    None => None,
                })
                .link(match event.attributes.get("link") {
                    Some(value) => Some(ids.resolve(&value.as_string())?),
                    None => None,
                })
                .reason(
                    event
                        .attributes
                        .get("reason")
                        .map(|value| value.as_string()),
                )
                .attributes(attrs)
                .build()
                .unwrap(),
        )
    }
}

#[cfg(test)]
mod tests {
    use crate::simulation::events::{
        EventTrait, EventsManager, Id, InternalAttributes, PersonArrivalEvent, PersonDepartureEvent,
    };
    use crate::simulation::time::SimTime;
    use macros::deterministic_id_test;
    use macros::event_struct;
    use std::cell::RefCell;
    use std::rc::Rc;

    // create new event type to make sure it works for new types as well
    #[event_struct]
    struct NewSimpleEvent {
        time: SimTime,
        some_field: String,
        attributes: InternalAttributes,
    }

    impl NewSimpleEvent {
        pub const TYPE: &'static str = "new simple event";
    }

    #[deterministic_id_test]
    fn test_events_manager() {
        let mut events_manager = EventsManager::new();

        // define vectors that we will make the events manager fill when processing events.
        let collection_of_person_arrival_events: Rc<RefCell<Vec<PersonArrivalEvent>>> =
            Rc::new(RefCell::new(Vec::new()));

        let collection_of_new_simple_events: Rc<RefCell<Vec<NewSimpleEvent>>> =
            Rc::new(RefCell::new(Vec::new()));

        let collection_of_any_event_strings: Rc<RefCell<Vec<String>>> =
            Rc::new(RefCell::new(Vec::new()));

        let collection_of_finish_strings: Rc<RefCell<Vec<String>>> =
            Rc::new(RefCell::new(Vec::new()));

        let collection_of_reset_iterations: Rc<RefCell<Vec<u32>>> =
            Rc::new(RefCell::new(Vec::new()));

        // clone the pointers, since we need to move them to the event managers
        let cloned_ptr_to_pa_collection = collection_of_person_arrival_events.clone();
        let cloned_ptr_to_nse_collection = collection_of_new_simple_events.clone();
        let cloned_ptr_to_any_collection = collection_of_any_event_strings.clone();
        let cloned_ptr_to_finish_collection = collection_of_finish_strings.clone();
        let cloned_ptr_to_reset_collection = collection_of_reset_iterations.clone();

        // register functions in event manager:

        // this registers a function in the events manager s.t. when a PersonArrivalEvent is
        // processed, it will be added to the collection_of_person_arrival_events vector.
        events_manager.on::<PersonArrivalEvent, _>(move |event| {
            cloned_ptr_to_pa_collection.borrow_mut().push(event.clone());
        });

        // this does the same for NewSimpleEvent, to make sure that it works for new event types as well.
        events_manager.on::<NewSimpleEvent, _>(move |event| {
            cloned_ptr_to_nse_collection
                .borrow_mut()
                .push(event.clone());
        });

        // this registers a function in the events manager s.t. when any event is processed, its
        // type will be added to the collection_of_any_events vector.
        events_manager.on_any(move |event: &dyn EventTrait| {
            cloned_ptr_to_any_collection
                .borrow_mut()
                .push(String::from(event.type_())); // cannot clone event without knowing
            // type, so we just store the type here,
            // for testing
        });

        // this registers a function in the events manager s.t. when the finish function is called,
        // "finished" will be added to the collection_of_finish_strings vector.
        events_manager.on_finish(move || {
            cloned_ptr_to_finish_collection
                .borrow_mut()
                .push(String::from("finished"));
        });

        events_manager.on_reset_iteration(move |iteration| {
            cloned_ptr_to_reset_collection.borrow_mut().push(iteration);
        });

        // create example events
        let event1 = PersonArrivalEvent {
            time: SimTime::from_secs(10),
            person: Id::create("person1"),
            link: Id::create("link1"),
            leg_mode: Id::create("car"),
            attributes: InternalAttributes::default(),
        };
        let event2 = PersonArrivalEvent {
            time: SimTime::from_secs(12),
            person: Id::create("person1"),
            link: Id::create("link1"),
            leg_mode: Id::create("car"),
            attributes: InternalAttributes::default(),
        };
        let event3 = PersonDepartureEvent {
            time: SimTime::from_secs(15),
            person: Id::create("person1"),
            link: Id::create("link1"),
            leg_mode: Id::create("car"),
            routing_mode: Id::create("fastest"),
            attributes: InternalAttributes::default(),
        };
        let event4 = NewSimpleEvent {
            time: SimTime::from_secs(20),
            some_field: String::from("some value"),
            attributes: InternalAttributes::default(),
        };

        // process all events and finish
        events_manager.process_event(&event1);
        events_manager.process_event(&event2);
        events_manager.process_event(&event3);
        events_manager.process_event(&event4);
        events_manager.reset_iteration(7);
        events_manager.process_event(&event1);
        events_manager.finish();

        // verify that the PA collection contains event1 and event2, but not event3 or event4
        assert_eq!(
            *collection_of_person_arrival_events.borrow(),
            vec![event1.clone(), event2, event1]
        );

        // verify that the NewSimpleEvent collection contains event4, but not the other events
        assert_eq!(*collection_of_new_simple_events.borrow(), vec![event4]);

        // verify that the "any" collection contains the types of all events
        assert_eq!(
            *collection_of_any_event_strings.borrow(),
            vec![
                String::from("arrival"),
                String::from("arrival"),
                String::from("departure"),
                String::from("new simple event"),
                String::from("arrival")
            ]
        );

        // verify that the finish collection contains "finished"
        assert_eq!(
            *collection_of_finish_strings.borrow(),
            vec![String::from("finished")]
        );

        assert_eq!(*collection_of_reset_iterations.borrow(), vec![7]);
    }
}
