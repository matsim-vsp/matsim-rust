use crate::generated::events::{GenericEvent, TimeStep};
use crate::generated::general::AttributeValue;
use crate::simulation::events::{
    ActivityEndEvent, ActivityStartEvent, AgentWaitingForPtEvent, EventHandlerRegisterFn,
    EventTrait, EventsManager, LinkEnterEvent, LinkLeaveEvent, PersonArrivalEvent,
    PersonDepartureEvent, PersonEntersVehicleEvent, PersonLeavesVehicleEvent, PersonStuckEvent,
    PtTeleportationArrivalEvent, TeleportationArrivalEvent, TransitDriverStartsEvent,
    VehicleArrivesAtFacilityEvent, VehicleDepartsAtFacilityEvent, VehicleEntersTrafficEvent,
    VehicleLeavesTrafficEvent,
};
use crate::simulation::id::{CreateMissingIds, ExistingIds, IdResolver};
use crate::simulation::io::batch::BatchPipeline;
use crate::simulation::io::proto::read_length_delimited;
use crate::simulation::time::SimTime;
use prost::Message;
use std::cell::RefCell;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;
use std::rc::Rc;

impl From<&ActivityEndEvent> for GenericEvent {
    fn from(value: &ActivityEndEvent) -> Self {
        let mut attributes = HashMap::new();
        attributes.insert(
            String::from("person"),
            AttributeValue::from(value.person.external()),
        );
        attributes.insert(
            String::from("link"),
            AttributeValue::from(value.link.external()),
        );
        attributes.insert(
            String::from("act_type"),
            AttributeValue::from(value.act_type.external()),
        );
        attributes.insert(String::from("x"), AttributeValue::from(value.coordinate.x));
        attributes.insert(String::from("y"), AttributeValue::from(value.coordinate.y));
        attributes.insert(String::from("z"), AttributeValue::from(value.coordinate.z));

        GenericEvent {
            r#type: value.type_().to_string(),
            attributes,
        }
    }
}

impl From<&ActivityStartEvent> for GenericEvent {
    fn from(value: &ActivityStartEvent) -> Self {
        let mut attributes = HashMap::new();
        attributes.insert(
            "person".to_string(),
            AttributeValue::from(value.person.external()),
        );
        attributes.insert(
            "link".to_string(),
            AttributeValue::from(value.link.external()),
        );
        attributes.insert(
            "act_type".to_string(),
            AttributeValue::from(value.act_type.external()),
        );
        attributes.insert(String::from("x"), AttributeValue::from(value.coordinate.x));
        attributes.insert(String::from("y"), AttributeValue::from(value.coordinate.y));
        attributes.insert(String::from("z"), AttributeValue::from(value.coordinate.z));

        GenericEvent {
            r#type: value.type_().to_string(),
            attributes,
        }
    }
}

impl From<&LinkEnterEvent> for GenericEvent {
    fn from(value: &LinkEnterEvent) -> Self {
        let mut attributes = HashMap::new();
        attributes.insert(
            "link".to_string(),
            AttributeValue::from(value.link.external()),
        );
        attributes.insert(
            "vehicle".to_string(),
            AttributeValue::from(value.vehicle.external()),
        );
        GenericEvent {
            r#type: value.type_().to_string(),
            attributes,
        }
    }
}

impl From<&LinkLeaveEvent> for GenericEvent {
    fn from(value: &LinkLeaveEvent) -> Self {
        let mut attributes = HashMap::new();
        attributes.insert(
            "link".to_string(),
            AttributeValue::from(value.link.external()),
        );
        attributes.insert(
            "vehicle".to_string(),
            AttributeValue::from(value.vehicle.external()),
        );
        GenericEvent {
            r#type: value.type_().to_string(),
            attributes,
        }
    }
}

impl From<&PersonEntersVehicleEvent> for GenericEvent {
    fn from(value: &PersonEntersVehicleEvent) -> Self {
        let mut attributes = HashMap::new();
        attributes.insert(
            "person".to_string(),
            AttributeValue::from(value.person.external()),
        );
        attributes.insert(
            "vehicle".to_string(),
            AttributeValue::from(value.vehicle.external()),
        );
        GenericEvent {
            r#type: value.type_().to_string(),
            attributes,
        }
    }
}

impl From<&PersonLeavesVehicleEvent> for GenericEvent {
    fn from(value: &PersonLeavesVehicleEvent) -> Self {
        let mut attributes = HashMap::new();
        attributes.insert(
            "person".to_string(),
            AttributeValue::from(value.person.external()),
        );
        attributes.insert(
            "vehicle".to_string(),
            AttributeValue::from(value.vehicle.external()),
        );
        GenericEvent {
            r#type: value.type_().to_string(),
            attributes,
        }
    }
}

impl From<&PersonDepartureEvent> for GenericEvent {
    fn from(value: &PersonDepartureEvent) -> Self {
        let mut attributes = HashMap::new();
        attributes.insert(
            "person".to_string(),
            AttributeValue::from(value.person.external()),
        );
        attributes.insert(
            "link".to_string(),
            AttributeValue::from(value.link.external()),
        );
        attributes.insert(
            "mode".to_string(),
            AttributeValue::from(value.leg_mode.external()),
        );
        attributes.insert(
            "routing_mode".to_string(),
            AttributeValue::from(value.routing_mode.external()),
        );
        GenericEvent {
            r#type: value.type_().to_string(),
            attributes,
        }
    }
}

impl From<&PersonArrivalEvent> for GenericEvent {
    fn from(value: &PersonArrivalEvent) -> Self {
        let mut attributes = HashMap::new();
        attributes.insert(
            "person".to_string(),
            AttributeValue::from(value.person.external()),
        );
        attributes.insert(
            "link".to_string(),
            AttributeValue::from(value.link.external()),
        );
        attributes.insert(
            "mode".to_string(),
            AttributeValue::from(value.leg_mode.external()),
        );
        GenericEvent {
            r#type: value.type_().to_string(),
            attributes,
        }
    }
}

impl From<&TeleportationArrivalEvent> for GenericEvent {
    fn from(value: &TeleportationArrivalEvent) -> Self {
        let mut attributes = HashMap::new();
        attributes.insert(
            "person".to_string(),
            AttributeValue::from(value.person.external()),
        );
        attributes.insert(
            "mode".to_string(),
            AttributeValue::from(value.mode.external()),
        );
        attributes.insert(
            "distance".to_string(),
            AttributeValue::from(value.distance.to_string()),
        );
        GenericEvent {
            r#type: value.type_().to_string(),
            attributes,
        }
    }
}

impl From<&PtTeleportationArrivalEvent> for GenericEvent {
    fn from(value: &PtTeleportationArrivalEvent) -> Self {
        let mut attributes = HashMap::new();
        attributes.insert(
            "person".to_string(),
            AttributeValue::from(value.person.external()),
        );
        attributes.insert(
            "mode".to_string(),
            AttributeValue::from(value.mode.external()),
        );
        attributes.insert(
            "distance".to_string(),
            AttributeValue::from(value.distance.to_string()),
        );
        attributes.insert(
            "route".to_string(),
            AttributeValue::from(value.route.external()),
        );
        attributes.insert(
            "line".to_string(),
            AttributeValue::from(value.line.external()),
        );
        attributes.insert(
            "boardingTimeNs".to_string(),
            AttributeValue::from(value.boarding_time.as_nanos().to_string()),
        );
        attributes.insert(
            "accessFacility".to_string(),
            AttributeValue::from(value.access_facility.external()),
        );
        attributes.insert(
            "egressFacility".to_string(),
            AttributeValue::from(value.egress_facility.external()),
        );
        GenericEvent {
            r#type: value.type_().to_string(),
            attributes,
        }
    }
}

impl From<&VehicleEntersTrafficEvent> for GenericEvent {
    fn from(value: &VehicleEntersTrafficEvent) -> Self {
        let mut attributes = HashMap::new();
        attributes.insert(
            "vehicle".to_string(),
            AttributeValue::from(value.vehicle.external()),
        );
        attributes.insert(
            "link".to_string(),
            AttributeValue::from(value.link.external()),
        );
        attributes.insert(
            "person".to_string(),
            AttributeValue::from(value.person.external()),
        );
        attributes.insert(
            "network_mode".to_string(),
            AttributeValue::from(value.network_mode.external()),
        );
        attributes.insert(
            "relative_position".to_string(),
            AttributeValue::from(value.relative_position),
        );
        GenericEvent {
            r#type: value.type_().to_string(),
            attributes,
        }
    }
}

impl From<&VehicleLeavesTrafficEvent> for GenericEvent {
    fn from(value: &VehicleLeavesTrafficEvent) -> Self {
        let mut attributes = HashMap::new();
        attributes.insert(
            "vehicle".to_string(),
            AttributeValue::from(value.vehicle.external()),
        );
        attributes.insert(
            "link".to_string(),
            AttributeValue::from(value.link.external()),
        );
        attributes.insert(
            "person".to_string(),
            AttributeValue::from(value.person.external()),
        );
        attributes.insert(
            "network_mode".to_string(),
            AttributeValue::from(value.network_mode.external()),
        );
        attributes.insert(
            "relative_position".to_string(),
            AttributeValue::from(value.relative_position),
        );
        GenericEvent {
            r#type: value.type_().to_string(),
            attributes,
        }
    }
}

impl From<&PersonStuckEvent> for GenericEvent {
    fn from(value: &PersonStuckEvent) -> Self {
        let mut attributes = HashMap::new();
        attributes.insert(
            "person".to_string(),
            AttributeValue::from(value.person.external()),
        );
        if let Some(link) = &value.link {
            attributes.insert("link".to_string(), AttributeValue::from(link.external()));
        }
        if let Some(leg_mode) = &value.leg_mode {
            attributes.insert(
                "leg_mode".to_string(),
                AttributeValue::from(leg_mode.external()),
            );
        }
        if let Some(reason) = &value.reason {
            attributes.insert("reason".to_string(), AttributeValue::from(reason.as_str()));
        }
        GenericEvent {
            r#type: value.type_().to_string(),
            attributes,
        }
    }
}

impl From<&crate::simulation::events::GenericEvent> for GenericEvent {
    fn from(value: &crate::simulation::events::GenericEvent) -> Self {
        let mut attributes = HashMap::new();
        for (k, v) in value.attributes.iter() {
            attributes.insert(k.clone(), AttributeValue::from(v.to_string()));
        }
        GenericEvent {
            r#type: value.type_().to_string(),
            attributes,
        }
    }
}

pub struct ProtoEventsWriter {
    encoded_events: Vec<u8>,
    curr_time_step: u64,
    writer: BufWriter<File>,
}

impl ProtoEventsWriter {
    pub fn new(path: impl AsRef<Path>) -> Self {
        let file = File::create(path).unwrap();
        let writer = BufWriter::new(file);
        ProtoEventsWriter {
            curr_time_step: 0,
            encoded_events: Vec::new(),
            writer,
        }
    }

    fn update_time_step(&mut self, time: SimTime) {
        let time = time.as_nanos();
        if self.curr_time_step != time {
            if !self.encoded_events.is_empty() {
                self.write_time_step();
            }
            self.curr_time_step = time;
        }
    }

    fn write_time_step(&mut self) {
        let mut data: Vec<u8> = Vec::with_capacity(self.encoded_events.len());
        std::mem::swap(&mut data, &mut self.encoded_events);

        let time_step = TimeStep {
            time_ns: self.curr_time_step,
            data,
        };
        let encoded_time_step = time_step.encode_length_delimited_to_vec();

        self.writer
            .write_all(&encoded_time_step)
            .expect("Failed to write all bytes");
    }

    pub(crate) fn on_any(&mut self, event: &dyn EventTrait) {
        self.update_time_step(event.time());
        let event = event_to_proto(event);

        event
            .encode_length_delimited(&mut self.encoded_events)
            .expect("Error encoding event.");
    }

    pub(crate) fn finish(&mut self) {
        self.write_time_step();
        self.writer
            .flush()
            .expect("Failed to flush buffered writer.");
    }

    /// Creates a register function that registers event handlers to an [EventsManager].
    /// This function takes a file path as an input and returns a boxed [EventHandlerRegisterFn]
    /// which can be used to register specific handlers to an [EventsManager]. The handlers
    /// allow the processing of events and the proper management of their lifecycle.
    pub fn register_fn(path: impl AsRef<Path> + Send + 'static) -> Box<EventHandlerRegisterFn> {
        Box::new(move |events: &mut EventsManager| {
            let proto = Rc::new(RefCell::new(ProtoEventsWriter::new(path)));
            let proto1 = proto.clone();
            let proto2 = proto.clone();

            events.on_any(move |e| {
                proto1.borrow_mut().on_any(e);
            });
            events.on_finish(move || {
                proto2.borrow_mut().finish();
            });
        })
    }
}

impl From<&TransitDriverStartsEvent> for GenericEvent {
    fn from(value: &TransitDriverStartsEvent) -> Self {
        let mut attributes = HashMap::new();
        for (key, id) in [
            ("driverId", value.driver.external()),
            ("vehicleId", value.vehicle.external()),
            ("transitLineId", value.line.external()),
            ("transitRouteId", value.route.external()),
            ("departureId", value.departure.external()),
        ] {
            attributes.insert(key.to_string(), AttributeValue::from(id));
        }
        GenericEvent {
            r#type: value.type_().to_string(),
            attributes,
        }
    }
}

impl From<&VehicleArrivesAtFacilityEvent> for GenericEvent {
    fn from(value: &VehicleArrivesAtFacilityEvent) -> Self {
        facility_event(
            value.type_(),
            value.vehicle.external(),
            value.facility.external(),
            value.delay,
        )
    }
}

impl From<&VehicleDepartsAtFacilityEvent> for GenericEvent {
    fn from(value: &VehicleDepartsAtFacilityEvent) -> Self {
        facility_event(
            value.type_(),
            value.vehicle.external(),
            value.facility.external(),
            value.delay,
        )
    }
}

fn facility_event(type_: &str, vehicle: &str, facility: &str, delay: f64) -> GenericEvent {
    let mut attributes = HashMap::new();
    attributes.insert("vehicle".to_string(), AttributeValue::from(vehicle));
    attributes.insert("facility".to_string(), AttributeValue::from(facility));
    attributes.insert("delay".to_string(), AttributeValue::from(delay));
    GenericEvent {
        r#type: type_.to_string(),
        attributes,
    }
}

impl From<&AgentWaitingForPtEvent> for GenericEvent {
    fn from(value: &AgentWaitingForPtEvent) -> Self {
        let mut attributes = HashMap::new();
        attributes.insert(
            "person".to_string(),
            AttributeValue::from(value.person.external()),
        );
        attributes.insert(
            "atStop".to_string(),
            AttributeValue::from(value.at_stop.external()),
        );
        attributes.insert(
            "destinationStop".to_string(),
            AttributeValue::from(value.destination_stop.external()),
        );
        GenericEvent {
            r#type: value.type_().to_string(),
            attributes,
        }
    }
}

pub(crate) fn event_to_proto(event: &dyn EventTrait) -> GenericEvent {
    if let Some(event) = event
        .as_any()
        .downcast_ref::<crate::simulation::events::GenericEvent>()
    {
        GenericEvent::from(event)
    } else if let Some(event) = event.as_any().downcast_ref::<ActivityStartEvent>() {
        GenericEvent::from(event)
    } else if let Some(event) = event.as_any().downcast_ref::<ActivityEndEvent>() {
        GenericEvent::from(event)
    } else if let Some(event) = event.as_any().downcast_ref::<LinkEnterEvent>() {
        GenericEvent::from(event)
    } else if let Some(event) = event.as_any().downcast_ref::<LinkLeaveEvent>() {
        GenericEvent::from(event)
    } else if let Some(event) = event.as_any().downcast_ref::<PersonEntersVehicleEvent>() {
        GenericEvent::from(event)
    } else if let Some(event) = event.as_any().downcast_ref::<PersonLeavesVehicleEvent>() {
        GenericEvent::from(event)
    } else if let Some(event) = event.as_any().downcast_ref::<PersonDepartureEvent>() {
        GenericEvent::from(event)
    } else if let Some(event) = event.as_any().downcast_ref::<PersonArrivalEvent>() {
        GenericEvent::from(event)
    } else if let Some(event) = event.as_any().downcast_ref::<TeleportationArrivalEvent>() {
        GenericEvent::from(event)
    } else if let Some(event) = event.as_any().downcast_ref::<PtTeleportationArrivalEvent>() {
        GenericEvent::from(event)
    } else if let Some(event) = event.as_any().downcast_ref::<VehicleEntersTrafficEvent>() {
        GenericEvent::from(event)
    } else if let Some(event) = event.as_any().downcast_ref::<VehicleLeavesTrafficEvent>() {
        GenericEvent::from(event)
    } else if let Some(event) = event.as_any().downcast_ref::<PersonStuckEvent>() {
        GenericEvent::from(event)
    } else if let Some(event) = event.as_any().downcast_ref::<TransitDriverStartsEvent>() {
        GenericEvent::from(event)
    } else if let Some(event) = event
        .as_any()
        .downcast_ref::<VehicleArrivesAtFacilityEvent>()
    {
        GenericEvent::from(event)
    } else if let Some(event) = event
        .as_any()
        .downcast_ref::<VehicleDepartsAtFacilityEvent>()
    {
        GenericEvent::from(event)
    } else if let Some(event) = event.as_any().downcast_ref::<AgentWaitingForPtEvent>() {
        GenericEvent::from(event)
    } else {
        // TODO use general event here and log warning
        panic!("Unknown event type: {:?}", event);
    }
}

/// Proto events are decoded in batches of about this many bytes. Decoded events need a multiple of
/// their encoded size, so the batches are smaller than for other inputs.
///
/// Test builds use small batches, so that test inputs consist of many batches.
const EVENT_BATCH_BYTES: usize = if cfg!(test) { 4 * 1024 } else { 1024 * 1024 };

/// Reads the time steps of a proto events file in file order.
///
/// The reader reads the input on a separate thread and decodes the time steps in parallel, see
/// [`BatchPipeline`].
pub struct ProtoEventsReader {
    pipeline: BatchPipeline<(SimTime, Vec<GenericEvent>)>,
}

impl ProtoEventsReader {
    pub fn new(reader: impl Read + Send + 'static) -> Self {
        Self {
            pipeline: time_step_pipeline(reader, decode_time_step),
        }
    }

    pub fn from_file(path: &Path) -> Self {
        Self::new(open_events_file(path))
    }
}

impl Iterator for ProtoEventsReader {
    type Item = (SimTime, Vec<GenericEvent>);

    fn next(&mut self) -> Option<Self::Item> {
        // The writer only writes an empty time step for files without any events. It carries no
        // information, so it is skipped.
        self.pipeline
            .by_ref()
            .find(|(_, events)| !events.is_empty())
    }
}

fn open_events_file(path: &Path) -> File {
    File::open(path).unwrap_or_else(|_e| panic!("Failed to open File at: {path:?}"))
}

/// Reads the time steps from `reader` on a separate thread and applies `transform` to the encoded
/// time steps in parallel.
fn time_step_pipeline<T: Send + 'static>(
    reader: impl Read + Send + 'static,
    transform: impl Fn(&[u8]) -> T + Send + Sync + 'static,
) -> BatchPipeline<T> {
    BatchPipeline::spawn(
        EVENT_BATCH_BYTES,
        move || BufReader::with_capacity(1024 * 1024, reader),
        |reader, buffer| read_length_delimited(reader, buffer),
        move |_, bytes| transform(bytes),
    )
}

fn decode_time_step(bytes: &[u8]) -> (SimTime, Vec<GenericEvent>) {
    let time_step = TimeStep::decode(bytes).expect("Could not decode TimeStep message");
    let mut data = time_step.data.as_slice();
    let mut events = Vec::new();
    while !data.is_empty() {
        let event = GenericEvent::decode_length_delimited(&mut data).expect("Error decoding event");
        events.push(event);
    }
    (SimTime::from_nanos(time_step.time_ns), events)
}

/// A proto event, which is already converted into an internal event if all its ids existed when
/// it was read.
pub(crate) enum PreparedEvent {
    Converted(Box<dyn EventTrait>),
    Pending(GenericEvent),
}

impl PreparedEvent {
    /// Calls `f` with the internal event. Pending events are converted, creating missing ids.
    pub(crate) fn with_event(&self, time: SimTime, f: impl FnOnce(&dyn EventTrait)) {
        match self {
            PreparedEvent::Converted(event) => f(event.as_ref()),
            PreparedEvent::Pending(proto_event) => f(event_from_proto(time, proto_event).as_ref()),
        }
    }
}

/// Reads the time steps of a proto events file like [`ProtoEventsReader::from_file`] and
/// additionally converts the events into internal events in parallel.
///
/// The parallel conversion only looks ids up. Events with ids which don't exist yet stay
/// pending. Converting them with [`PreparedEvent::with_event`] in processing order creates the
/// missing ids in the same order as converting all events one after another.
pub(crate) struct PreparedProtoEventsReader {
    pipeline: BatchPipeline<(SimTime, Vec<PreparedEvent>)>,
}

impl PreparedProtoEventsReader {
    pub(crate) fn from_file(path: &Path) -> Self {
        let pipeline = time_step_pipeline(open_events_file(path), |bytes| {
            let (time, events) = decode_time_step(bytes);
            let prepared = events
                .into_iter()
                .map(
                    |event| match try_event_from_proto(time, &event, &ExistingIds) {
                        Some(converted) => PreparedEvent::Converted(converted),
                        None => PreparedEvent::Pending(event),
                    },
                )
                .collect();
            (time, prepared)
        });
        Self { pipeline }
    }
}

impl Iterator for PreparedProtoEventsReader {
    type Item = (SimTime, Vec<PreparedEvent>);

    fn next(&mut self) -> Option<Self::Item> {
        // Skip empty time steps like `ProtoEventsReader` does.
        self.pipeline
            .by_ref()
            .find(|(_, events)| !events.is_empty())
    }
}

pub fn process_events(time: SimTime, events: &Vec<GenericEvent>, manager: &mut EventsManager) {
    for proto_event in events {
        let internal_event = event_from_proto(time, proto_event);
        manager.process_event(internal_event.as_ref());
    }
}

pub(crate) fn event_from_proto(time: SimTime, proto_event: &GenericEvent) -> Box<dyn EventTrait> {
    try_event_from_proto(time, proto_event, &CreateMissingIds)
        .expect("Creating missing ids never fails.")
}

/// Like [`event_from_proto`], but returns `None` if `ids` doesn't resolve an id.
#[rustfmt::skip]
pub(crate) fn try_event_from_proto(
    time: SimTime,
    proto_event: &GenericEvent,
    ids: &impl IdResolver,
) -> Option<Box<dyn EventTrait>> {
    let type_ = proto_event.r#type.as_str();
    let event: Box<dyn EventTrait> = match type_ {
        crate::simulation::events::GenericEvent::TYPE => Box::new(crate::simulation::events::GenericEvent::try_from_proto_event(proto_event, time, ids)?),
        ActivityStartEvent::TYPE => Box::new(ActivityStartEvent::try_from_proto_event(proto_event, time, ids)?),
        ActivityEndEvent::TYPE => Box::new(ActivityEndEvent::try_from_proto_event(proto_event, time, ids)?),
        LinkEnterEvent::TYPE => Box::new(LinkEnterEvent::try_from_proto_event(proto_event, time, ids)?),
        LinkLeaveEvent::TYPE => Box::new(LinkLeaveEvent::try_from_proto_event(proto_event, time, ids)?),
        PersonEntersVehicleEvent::TYPE => Box::new(PersonEntersVehicleEvent::try_from_proto_event(proto_event, time, ids)?),
        PersonLeavesVehicleEvent::TYPE => Box::new(PersonLeavesVehicleEvent::try_from_proto_event(proto_event, time, ids)?),
        PersonDepartureEvent::TYPE => Box::new(PersonDepartureEvent::try_from_proto_event(proto_event, time, ids)?),
        PersonArrivalEvent::TYPE => Box::new(PersonArrivalEvent::try_from_proto_event(proto_event, time, ids)?),
        TeleportationArrivalEvent::TYPE => Box::new(TeleportationArrivalEvent::try_from_proto_event(proto_event, time, ids)?),
        PtTeleportationArrivalEvent::TYPE => Box::new(PtTeleportationArrivalEvent::try_from_proto_event(proto_event, time, ids)?),
        VehicleEntersTrafficEvent::TYPE => Box::new(VehicleEntersTrafficEvent::try_from_proto_event(proto_event, time, ids)?),
        VehicleLeavesTrafficEvent::TYPE => Box::new(VehicleLeavesTrafficEvent::try_from_proto_event(proto_event, time, ids)?),
        PersonStuckEvent::TYPE => Box::new(PersonStuckEvent::try_from_proto_event(proto_event, time, ids)?),
        TransitDriverStartsEvent::TYPE => Box::new(TransitDriverStartsEvent::from_proto_event(proto_event, time)),
        VehicleArrivesAtFacilityEvent::TYPE => Box::new(VehicleArrivesAtFacilityEvent::from_proto_event(proto_event, time)),
        VehicleDepartsAtFacilityEvent::TYPE => Box::new(VehicleDepartsAtFacilityEvent::from_proto_event(proto_event, time)),
        AgentWaitingForPtEvent::TYPE => Box::new(AgentWaitingForPtEvent::from_proto_event(proto_event, time)),
        _ => panic!("Unknown event type: {:?}", type_),
    };
    Some(event)
}

#[cfg(test)]
mod tests {
    use crate::generated::events::GenericEvent;
    use crate::simulation::InternalAttributes;
    use crate::simulation::events::{
        ActivityEndEvent, ActivityEndEventBuilder, ActivityStartEvent, ActivityStartEventBuilder,
        EventTrait, GenericEventBuilder, PersonStuckEvent, PersonStuckEventBuilder,
    };
    use crate::simulation::id::Id;
    use crate::simulation::io::proto::proto_events::{
        ProtoEventsReader, ProtoEventsWriter, event_from_proto, event_to_proto,
    };
    use crate::simulation::scenario::Coordinate;
    use crate::simulation::time::SimTime;
    use macros::deterministic_id_test;
    use std::collections::HashMap;
    use std::fs;
    use std::path::PathBuf;

    #[deterministic_id_test]
    fn person_stuck_proto_round_trip_preserves_optional_attributes() {
        let time = SimTime::from_secs(42);
        let with_optional_attributes = PersonStuckEventBuilder::default()
            .time(time)
            .person(Id::create("person-with-details"))
            .link(Some(Id::create("link-1")))
            .leg_mode(Some(Id::create("car")))
            .reason(Some("mobsim end".to_string()))
            .build()
            .unwrap();
        let without_optional_attributes = PersonStuckEventBuilder::default()
            .time(time)
            .person(Id::create("person-without-details"))
            .build()
            .unwrap();

        let with_optional_proto = event_to_proto(&with_optional_attributes);
        assert_eq!("link-1", with_optional_proto.attributes["link"].as_string());
        assert_eq!(
            "car",
            with_optional_proto.attributes["leg_mode"].as_string()
        );
        assert_eq!(
            "mobsim end",
            with_optional_proto.attributes["reason"].as_string()
        );

        let without_optional_proto = event_to_proto(&without_optional_attributes);
        assert_eq!(1, without_optional_proto.attributes.len());
        assert!(without_optional_proto.attributes.contains_key("person"));
        assert!(!without_optional_proto.attributes.contains_key("link"));
        assert!(!without_optional_proto.attributes.contains_key("leg_mode"));
        assert!(!without_optional_proto.attributes.contains_key("reason"));

        for (expected, proto) in [
            (&with_optional_attributes, &with_optional_proto),
            (&without_optional_attributes, &without_optional_proto),
        ] {
            let parsed_event = event_from_proto(time, proto);
            let parsed_event = parsed_event
                .as_any()
                .downcast_ref::<PersonStuckEvent>()
                .unwrap();
            assert_eq!(expected.time, parsed_event.time);
            assert_eq!(expected.person, parsed_event.person);
            assert_eq!(expected.link, parsed_event.link);
            assert_eq!(expected.leg_mode, parsed_event.leg_mode);
            assert_eq!(expected.reason, parsed_event.reason);
        }
    }

    #[deterministic_id_test]
    fn write_read_single() {
        let path =
            create_path_with_prefix("./test_output/io/proto_events/write_read_single/events.pbf");
        let mut writer = ProtoEventsWriter::new(&path);
        let event: Box<dyn EventTrait> = Box::new(
            GenericEventBuilder::default()
                .time(SimTime::from_nanos(1_500_000))
                .attributes(InternalAttributes::from(HashMap::from([(
                    String::from("attr1"),
                    String::from("value1"),
                )])))
                .build()
                .unwrap(),
        );
        writer.on_any(event.as_ref());
        writer.finish();

        // now read in
        let mut reader = ProtoEventsReader::from_file(&path);
        let (time, events) = reader.next().expect("Couldn't read timestep.");
        assert_eq!(SimTime::from_nanos(1_500_000), time);
        assert_eq!(1, events.len());
        match_events(&event, events.first().unwrap());
    }

    #[deterministic_id_test]
    fn write_read_multiple() {
        let path =
            create_path_with_prefix("./test_output/io/proto_events/write_read_multiple/events.pbf");
        let mut writer = ProtoEventsWriter::new(&path);
        let issued_events: Vec<Box<dyn EventTrait>> = vec![
            Box::new(
                GenericEventBuilder::default()
                    .time(SimTime::from_secs(103))
                    .attributes(InternalAttributes::from(HashMap::from([(
                        String::from("attr1"),
                        String::from("value1"),
                    )])))
                    .build()
                    .unwrap(),
            ),
            Box::new(
                ActivityStartEventBuilder::default()
                    .time(SimTime::from_secs(103))
                    .person(Id::create("1"))
                    .link(Id::create("1"))
                    .act_type(Id::create("1"))
                    .coordinate(Coordinate::default())
                    .build()
                    .unwrap(),
            ),
            Box::new(
                ActivityEndEventBuilder::default()
                    .time(SimTime::from_secs(103))
                    .person(Id::create("1"))
                    .link(Id::create("1"))
                    .coordinate(Coordinate::default())
                    .act_type(Id::create("1"))
                    .build()
                    .unwrap(),
            ),
        ];

        for event in &issued_events {
            writer.on_any(event.as_ref());
        }
        writer.finish();

        // now read in
        let mut reader = ProtoEventsReader::from_file(&path);
        let (time, events) = reader.next().expect("Couldn't read timestep.");
        assert_eq!(SimTime::from_secs(103), time);
        assert_eq!(issued_events.len(), events.len());

        for (i, expected_event) in issued_events.iter().enumerate() {
            match_events(expected_event, events.get(i).unwrap());
        }
    }

    #[deterministic_id_test]
    fn write_read_multiple_time_steps() {
        let path = create_path_with_prefix(
            "./test_output/io/proto_events/write_read_multiple_time_steps/events.pbf",
        );

        let mut writer = ProtoEventsWriter::new(&path);

        let mut issued_events: Vec<Box<dyn EventTrait>> = Vec::new();

        for time_step in 43..109 {
            let mut v: Vec<Box<dyn EventTrait>> = vec![
                Box::new(
                    GenericEventBuilder::default()
                        .time(SimTime::from_secs(time_step))
                        .attributes(InternalAttributes::from(HashMap::from([(
                            String::from("attr1"),
                            String::from("value1"),
                        )])))
                        .build()
                        .unwrap(),
                ),
                Box::new(
                    ActivityStartEventBuilder::default()
                        .time(SimTime::from_secs(time_step))
                        .person(Id::create("1"))
                        .link(Id::create("1"))
                        .act_type(Id::create("1"))
                        .coordinate(Coordinate::default())
                        .build()
                        .unwrap(),
                ),
                Box::new(
                    ActivityEndEventBuilder::default()
                        .time(SimTime::from_secs(time_step))
                        .person(Id::create("1"))
                        .link(Id::create("1"))
                        .act_type(Id::create("1"))
                        .coordinate(Coordinate::default())
                        .build()
                        .unwrap(),
                ),
            ];
            issued_events.append(&mut v);
        }

        for event in &issued_events {
            writer.on_any(event.as_ref());
        }

        writer.finish();

        let reader = ProtoEventsReader::from_file(&path);
        let start_time = SimTime::from_secs(43);
        let end_time = SimTime::from_secs(109);
        let mut last_time_step = SimTime::from_secs(42);
        for (time, events) in reader {
            // make sure times are in the correct range and order
            assert!(time >= start_time);
            assert!(time <= end_time);
            assert!(time > last_time_step);
            last_time_step = time;

            assert_eq!(3, events.len());
            for (i, event) in events.iter().enumerate() {
                let index = ((time.as_secs() - start_time.as_secs()) * 3) as usize + i;
                match_events(issued_events.get(index).unwrap(), event);
            }
        }
    }

    #[deterministic_id_test]
    fn reader_returns_all_time_steps_in_order() {
        let path = create_path_with_prefix(
            "./test_output/io/proto_events/reader_returns_all_time_steps_in_order/events.binpb",
        );
        let mut writer = ProtoEventsWriter::new(&path);
        let mut expected = Vec::new();
        for time_step in 0..500 {
            // Some time steps have no events, others many, so that batches end within and between
            // time steps.
            let mut events = Vec::new();
            for i in 0..time_step % 7 {
                let event = ActivityEndEventBuilder::default()
                    .time(SimTime::from_secs(time_step))
                    .person(Id::create(&format!("person {i}")))
                    .link(Id::create(&format!("link {time_step}")))
                    .act_type(Id::create("home"))
                    .coordinate(Coordinate::default())
                    .build()
                    .unwrap();
                writer.on_any(&event);
                events.push(event_to_proto(&event));
            }
            if !events.is_empty() {
                expected.push((SimTime::from_secs(time_step), events));
            }
        }
        writer.finish();

        let from_file: Vec<_> = ProtoEventsReader::from_file(&path).collect();
        assert_eq!(expected, from_file);
        let bytes = std::io::Cursor::new(fs::read(&path).unwrap());
        let from_reader: Vec<_> = ProtoEventsReader::new(bytes).collect();
        assert_eq!(expected, from_reader);
    }

    fn create_path_with_prefix(path: &str) -> PathBuf {
        // create path and corresponding directories
        let path_buf = PathBuf::from(path);
        let prefix = path_buf.parent().unwrap();
        fs::create_dir_all(prefix).unwrap();
        path_buf
    }

    fn match_events(event: &Box<dyn EventTrait>, other: &GenericEvent) {
        let type_ = event.type_();
        assert_eq!(type_, other.r#type);

        match type_ {
            crate::simulation::events::GenericEvent::TYPE => {
                let _typed_event = event
                    .as_any()
                    .downcast_ref::<crate::simulation::events::GenericEvent>()
                    .unwrap();
            }
            ActivityStartEvent::TYPE => {
                let typed_event = event.as_any().downcast_ref::<ActivityStartEvent>().unwrap();
                assert_eq!(
                    typed_event.person.external(),
                    other.attributes["person"].as_string()
                );
                assert_eq!(
                    typed_event.link.external(),
                    other.attributes["link"].as_string()
                );
                assert_eq!(
                    typed_event.act_type.external(),
                    other.attributes["act_type"].as_string()
                );
            }
            ActivityEndEvent::TYPE => {
                let typed_event = event.as_any().downcast_ref::<ActivityEndEvent>().unwrap();
                assert_eq!(
                    typed_event.person.external(),
                    other.attributes["person"].as_string()
                );
                assert_eq!(
                    typed_event.link.external(),
                    other.attributes["link"].as_string()
                );
                assert_eq!(
                    typed_event.act_type.external(),
                    other.attributes["act_type"].as_string()
                );
            }
            _ => panic!("wrong type"),
        }
    }
}
