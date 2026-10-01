use crate::generated::events::{GenericEvent, TimeStep};
use crate::generated::general::AttributeValue;
use crate::simulation::events::{
    ActivityEndEvent, ActivityStartEvent, EventHandlerRegisterFn, EventTrait, EventsManager,
    LinkEnterEvent, LinkLeaveEvent, PersonArrivalEvent, PersonDepartureEvent,
    PersonEntersVehicleEvent, PersonLeavesVehicleEvent, PersonStuckEvent,
    PtTeleportationArrivalEvent, TeleportationArrivalEvent, VehicleEntersTrafficEvent,
    VehicleLeavesTrafficEvent,
};
use crate::simulation::time::SimTime;
use prost::Message;
use std::cell::RefCell;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, BufWriter, Cursor, ErrorKind, Read, Seek, Write};
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
    } else {
        // TODO use general event here and log warning
        panic!("Unknown event type: {:?}", event);
    }
}

pub struct ProtoEventsReader<R: Read + Seek> {
    reader: BufReader<R>,
}

impl<R: Read + Seek> ProtoEventsReader<R> {
    pub fn new(reader: R) -> Self {
        ProtoEventsReader {
            reader: BufReader::new(reader),
        }
    }

    fn read_delim(&mut self) -> Option<usize> {
        // read the delimiter of the message. Prost says delimiter is between 1 and 10 bytes
        // so, read the first 10 bytes of the buffer
        let mut delim_buffer: [u8; 10] = [0; 10];

        // this could crash
        match self.reader.read_exact(&mut delim_buffer) {
            Ok(_) => {} // go on.
            Err(e) => match e.kind() {
                ErrorKind::UnexpectedEof => return None,
                _ => {
                    panic!("Error while reading file: {}", e);
                }
            },
        }
        let delimiter = prost::decode_length_delimiter(delim_buffer.as_slice())
            .expect("error reading delimiter");

        // since the delimiter is a varint figure out how many bytes the delimiter was actually taking
        // up in the buffer. Set the buffers position to the first byte after the delimiter, which
        // should be the start of the TimeStep message
        let delim_encoded_len = prost::encoding::encoded_len_varint(delimiter as u64) as i64;
        let offset = delim_encoded_len - (delim_buffer.len() as i64);
        self.reader
            .seek_relative(offset)
            .expect("Seeking relative failed");

        Some(delimiter)
    }

    fn read_time_step(&mut self, delimiter: usize) -> TimeStep {
        // allocate a buffer with the message length and read into it
        let mut msg_buffer: Vec<u8> = vec![0; delimiter];
        self.reader
            .read_exact(&mut msg_buffer)
            .expect("Error reading msg buffer");

        // then decode it.
        TimeStep::decode(msg_buffer.as_slice()).expect("Could not decode TimeStep message")
    }

    fn read_events(&mut self, time_step: TimeStep) -> Vec<GenericEvent> {
        let data_len = time_step.data.len() as u64;

        let mut cursor = Cursor::new(time_step.data);
        let mut result = Vec::new();

        while cursor.position() < data_len {
            let event =
                GenericEvent::decode_length_delimited(&mut cursor).expect("Error decoding event");
            result.push(event);
        }

        result
    }
}

impl<R: Read + Seek> Iterator for ProtoEventsReader<R> {
    type Item = (SimTime, Vec<GenericEvent>);

    fn next(&mut self) -> Option<Self::Item> {
        let delimiter = self.read_delim()?;
        let time_step = self.read_time_step(delimiter);
        let time = SimTime::from_nanos(time_step.time_ns);
        let events = self.read_events(time_step);

        Some((time, events))
    }
}

impl ProtoEventsReader<File> {
    pub fn from_file(path: &Path) -> Self {
        let file = File::open(path).unwrap_or_else(|_e| panic!("Failed to open File at: {path:?}"));
        Self::new(file)
    }
}

pub fn process_events(time: SimTime, events: &Vec<GenericEvent>, manager: &mut EventsManager) {
    for proto_event in events {
        let internal_event = event_from_proto(time, proto_event);
        manager.process_event(internal_event.as_ref());
    }
}

#[rustfmt::skip]
pub(crate) fn event_from_proto(time: SimTime, proto_event: &GenericEvent) -> Box<dyn EventTrait> {
    let type_ = proto_event.r#type.as_str();
    match type_ {
        crate::simulation::events::GenericEvent::TYPE => Box::new(crate::simulation::events::GenericEvent::from_proto_event(proto_event, time)),
        ActivityStartEvent::TYPE => Box::new(ActivityStartEvent::from_proto_event(proto_event, time)),
        ActivityEndEvent::TYPE => Box::new(ActivityEndEvent::from_proto_event(proto_event, time)),
        LinkEnterEvent::TYPE => Box::new(LinkEnterEvent::from_proto_event(proto_event, time)),
        LinkLeaveEvent::TYPE => Box::new(LinkLeaveEvent::from_proto_event(proto_event, time)),
        PersonEntersVehicleEvent::TYPE => Box::new(PersonEntersVehicleEvent::from_proto_event(proto_event, time)),
        PersonLeavesVehicleEvent::TYPE => Box::new(PersonLeavesVehicleEvent::from_proto_event(proto_event, time)),
        PersonDepartureEvent::TYPE => Box::new(PersonDepartureEvent::from_proto_event(proto_event, time)),
        PersonArrivalEvent::TYPE => Box::new(PersonArrivalEvent::from_proto_event(proto_event, time)),
        TeleportationArrivalEvent::TYPE => Box::new(TeleportationArrivalEvent::from_proto_event(proto_event, time)),
        PtTeleportationArrivalEvent::TYPE => Box::new(PtTeleportationArrivalEvent::from_proto_event(proto_event, time)),
        VehicleEntersTrafficEvent::TYPE => Box::new(VehicleEntersTrafficEvent::from_proto_event(proto_event, time)),
        VehicleLeavesTrafficEvent::TYPE => Box::new(VehicleLeavesTrafficEvent::from_proto_event(proto_event, time)),
        PersonStuckEvent::TYPE => Box::new(PersonStuckEvent::from_proto_event(proto_event, time)),
        _ => panic!("Unknown event type: {:?}", type_),
    }
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
