use flate2::Compression;
use flate2::write::GzEncoder;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;
use std::rc::Rc;
use std::sync::Mutex;
use tracing::info;
use xml::EventReader;
use xml::attribute::OwnedAttribute;
use xml::reader::XmlEvent;
use zstd::stream::read::Decoder as ZstdDecoder;
use zstd::stream::write::Encoder as ZstdEncoder;

use crate::simulation::events::{
    ActivityEndEvent, ActivityEndEventBuilder, ActivityStartEvent, ActivityStartEventBuilder,
    EventHandlerRegisterFn, EventTrait, EventsManager, GenericEvent, LinkEnterEvent,
    LinkEnterEventBuilder, LinkLeaveEvent, LinkLeaveEventBuilder, PersonArrivalEvent,
    PersonArrivalEventBuilder, PersonDepartureEvent, PersonDepartureEventBuilder,
    PersonEntersVehicleEvent, PersonEntersVehicleEventBuilder, PersonLeavesVehicleEvent,
    PersonLeavesVehicleEventBuilder, PersonStuckEvent, PersonStuckEventBuilder,
    PtTeleportationArrivalEvent, PtTeleportationArrivalEventBuilder, TeleportationArrivalEvent,
    TeleportationArrivalEventBuilder, VehicleEntersTrafficEvent, VehicleEntersTrafficEventBuilder,
    VehicleLeavesTrafficEvent, VehicleLeavesTrafficEventBuilder,
};
use crate::simulation::id::{CreateMissingIds, ExistingIds, Id, IdResolver};
use crate::simulation::io::batch::{BatchPipeline, ReadAhead};
use crate::simulation::io::xml::element_splitter::ElementSplitter;
use crate::simulation::scenario::Coordinate;
use crate::simulation::scenario::network::Link;
use crate::simulation::scenario::population::InternalPerson;
use crate::simulation::scenario::vehicles::InternalVehicle;
use crate::simulation::time::SimTime;

pub struct XmlEventsWriter {
    writer: Mutex<Option<XmlEventsOutputWriter>>,
}

enum XmlEventsOutputWriter {
    Plain(BufWriter<File>),
    Gz(GzEncoder<File>),
    Zst(ZstdEncoder<'static, File>),
}

impl XmlEventsOutputWriter {
    fn new(path: impl AsRef<Path>) -> Self {
        let file = File::create(&path).expect("Failed to create File.");
        match path.as_ref().extension().unwrap().to_str() {
            Some("gz") => Self::Gz(GzEncoder::new(file, Compression::fast())),
            Some("zst") => {
                Self::Zst(ZstdEncoder::new(file, 0).expect("Failed to create zstd encoder"))
            }
            _ => Self::Plain(BufWriter::new(file)),
        }
    }

    fn write_all(&mut self, bytes: &[u8]) {
        match self {
            Self::Plain(writer) => writer.write_all(bytes),
            Self::Gz(writer) => writer.write_all(bytes),
            Self::Zst(writer) => writer.write_all(bytes),
        }
        .expect("Error while writing event");
    }

    fn finish(self) {
        match self {
            Self::Plain(mut writer) => writer.flush().expect("Failed to flush events."),
            Self::Gz(writer) => {
                writer.finish().expect("Failed to finish gzip events.");
            }
            Self::Zst(writer) => {
                writer.finish().expect("Failed to finish zstd events.");
            }
        }
    }
}

impl XmlEventsWriter {
    pub fn new(path: impl AsRef<Path>) -> Self {
        info!("Creating file: {:?}", path.as_ref());
        let mut writer = XmlEventsOutputWriter::new(path);
        let header = "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<events version=\"1.0\">\n";
        writer.write_all(header.as_bytes());
        XmlEventsWriter {
            writer: Mutex::new(Some(writer)),
        }
    }

    pub fn event_2_string(e: &dyn EventTrait) -> String {
        if let Some(ev) = e.as_any().downcast_ref::<GenericEvent>() {
            format!(
                "<event time=\"{}\" type=\"{}\"/>\n",
                ev.time().format_decimal_seconds(),
                ev.type_()
            )
        } else if let Some(ev) = e.as_any().downcast_ref::<ActivityStartEvent>() {
            format!(
                "<event time=\"{}\" type=\"{}\" person=\"{}\" link=\"{}\" x=\"{}\" y=\"{}\" actType=\"{}\"/>\n",
                ev.time().format_decimal_seconds(),
                ev.type_(),
                ev.person,
                ev.link,
                ev.coordinate.x,
                ev.coordinate.y,
                ev.act_type
            )
        } else if let Some(ev) = e.as_any().downcast_ref::<ActivityEndEvent>() {
            format!(
                "<event time=\"{}\" type=\"{}\" person=\"{}\" link=\"{}\" x=\"{}\" y=\"{}\" actType=\"{}\"/>\n",
                ev.time().format_decimal_seconds(),
                ev.type_(),
                ev.person,
                ev.link,
                ev.coordinate.x,
                ev.coordinate.y,
                ev.act_type
            )
        } else if let Some(ev) = e.as_any().downcast_ref::<LinkEnterEvent>() {
            format!(
                "<event time=\"{}\" type=\"{}\" link=\"{}\" vehicle=\"{}\"/>\n",
                ev.time().format_decimal_seconds(),
                ev.type_(),
                ev.link,
                ev.vehicle
            )
        } else if let Some(ev) = e.as_any().downcast_ref::<LinkLeaveEvent>() {
            format!(
                "<event time=\"{}\" type=\"{}\" link=\"{}\" vehicle=\"{}\"/>\n",
                ev.time().format_decimal_seconds(),
                ev.type_(),
                ev.link,
                ev.vehicle
            )
        } else if let Some(ev) = e.as_any().downcast_ref::<PersonEntersVehicleEvent>() {
            format!(
                "<event time=\"{}\" type=\"{}\" person=\"{}\" vehicle=\"{}\"/>\n",
                ev.time().format_decimal_seconds(),
                ev.type_(),
                ev.person,
                ev.vehicle
            )
        } else if let Some(ev) = e.as_any().downcast_ref::<PersonLeavesVehicleEvent>() {
            format!(
                "<event time=\"{}\" type=\"{}\" person=\"{}\" vehicle=\"{}\"/>\n",
                ev.time().format_decimal_seconds(),
                ev.type_(),
                ev.person,
                ev.vehicle
            )
        } else if let Some(ev) = e.as_any().downcast_ref::<PersonDepartureEvent>() {
            format!(
                "<event time=\"{}\" type=\"{}\" person=\"{}\" link=\"{}\" legMode=\"{}\" computationalRoutingMode=\"{}\"/>\n",
                ev.time().format_decimal_seconds(),
                ev.type_(),
                ev.person,
                ev.link,
                ev.leg_mode,
                ev.routing_mode
            )
        } else if let Some(ev) = e.as_any().downcast_ref::<PersonArrivalEvent>() {
            format!(
                "<event time=\"{}\" type=\"{}\" person=\"{}\" link=\"{}\" legMode=\"{}\"/>\n",
                ev.time().format_decimal_seconds(),
                ev.type_(),
                ev.person,
                ev.link,
                ev.leg_mode
            )
        } else if let Some(ev) = e.as_any().downcast_ref::<TeleportationArrivalEvent>() {
            format!(
                "<event time=\"{}\" type=\"{}\" person=\"{}\" distance=\"{}\" mode=\"{}\"/>\n",
                ev.time().format_decimal_seconds(),
                ev.type_(),
                ev.person,
                ev.distance,
                ev.mode
            )
        } else if let Some(ev) = e.as_any().downcast_ref::<PtTeleportationArrivalEvent>() {
            format!(
                "<event time=\"{}\" type=\"{}\" person=\"{}\" distance=\"{}\" mode=\"{}\" line=\"{}\" route=\"{}\" boardingTime=\"{}\" accessFacility=\"{}\" egressFacility=\"{}\"/>\n",
                ev.time().format_decimal_seconds(),
                ev.type_(),
                ev.person,
                ev.distance,
                ev.mode,
                ev.line,
                ev.route,
                ev.boarding_time.format_decimal_seconds(),
                ev.access_facility,
                ev.egress_facility
            )
        } else if let Some(ev) = e.as_any().downcast_ref::<VehicleLeavesTrafficEvent>() {
            format!(
                "<event time=\"{}\" type=\"{}\" person=\"{}\" link=\"{}\" vehicle=\"{}\" networkMode=\"{}\" relativePosition=\"{}\"/>\n",
                ev.time().format_decimal_seconds(),
                ev.type_(),
                ev.person,
                ev.link,
                ev.vehicle,
                ev.network_mode,
                ev.relative_position
            )
        } else if let Some(ev) = e.as_any().downcast_ref::<VehicleEntersTrafficEvent>() {
            format!(
                "<event time=\"{}\" type=\"{}\" person=\"{}\" link=\"{}\" vehicle=\"{}\" networkMode=\"{}\" relativePosition=\"{}\"/>\n",
                ev.time().format_decimal_seconds(),
                ev.type_(),
                ev.person,
                ev.link,
                ev.vehicle,
                ev.network_mode,
                ev.relative_position
            )
        } else if let Some(stuck) = e.as_any().downcast_ref::<PersonStuckEvent>() {
            let mut result = format!(
                "<event time=\"{}\" type=\"{}\" person=\"{}\"",
                stuck.time().format_decimal_seconds(),
                stuck.type_(),
                stuck.person,
            );
            if let Some(link) = &stuck.link {
                result.push_str(&format!(" link=\"{link}\""));
            }
            if let Some(leg_mode) = &stuck.leg_mode {
                result.push_str(&format!(" legMode=\"{leg_mode}\""));
            }
            if let Some(reason) = &stuck.reason {
                result.push_str(&format!(" reason=\"{reason}\""));
            }
            result.push_str("/>\n");
            result
        } else {
            panic!("Unknown event type");
        }
    }

    pub fn on_any(&self, e: &dyn EventTrait) {
        self.write(&Self::event_2_string(e));
    }

    fn write(&self, text: &str) {
        let mut guard = self.writer.lock().expect("Failed to lock writer");
        let writer = guard
            .as_mut()
            .expect("Cannot write event after events writer was finished");
        writer.write_all(text.as_bytes());
    }

    pub fn finish(&self) {
        info!("Finishing Events File.");
        let mut guard = self.writer.lock().expect("Failed to lock writer");
        let Some(mut writer) = guard.take() else {
            return;
        };
        writer.write_all(b"</events>");
        writer.finish();
    }

    pub fn register_fn(path: impl AsRef<Path> + Send + 'static) -> Box<EventHandlerRegisterFn> {
        Box::new(move |events: &mut EventsManager| {
            let xml = Rc::new(XmlEventsWriter::new(path));
            let xml1 = xml.clone();
            let xml2 = xml.clone();

            events.on_any(move |e| {
                xml1.on_any(e);
            });
            events.on_finish(move || {
                xml2.finish();
            })
        })
    }
}

/// Events are parsed in batches of about this many bytes of XML.
///
/// Test builds use small batches, so that test inputs consist of many batches.
const EVENT_BATCH_BYTES: usize = if cfg!(test) { 4 * 1024 } else { 1024 * 1024 };

/// Reads the events of an XML events file in file order.
///
/// Separate threads decompress the file and split it into events, which are then parsed and
/// converted in parallel, see [`BatchPipeline`]. The conversion only looks ids up. Events with ids
/// which don't exist yet are converted by [`XmlEventsReader::read_next`], which creates the missing
/// ids in the same order as converting all events one after another.
///
/// Invalid input, e.g., a truncated file or an element which can't be parsed, panics.
pub struct XmlEventsReader {
    pipeline: BatchPipeline<ParsedEvent>,
}

/// An event, which is already converted if all its ids existed when it was parsed.
struct ParsedEvent {
    time: SimTime,
    event: Result<Box<dyn EventTrait>, Vec<OwnedAttribute>>,
}

impl XmlEventsReader {
    pub fn new(events_file: impl AsRef<Path>) -> Self {
        let path = events_file.as_ref().to_path_buf();
        let file =
            File::open(&path).unwrap_or_else(|_| panic!("Could not open events file: {:?}", path));
        let pipeline = BatchPipeline::spawn(
            EVENT_BATCH_BYTES,
            // Decompressing and splitting the input run on separate threads.
            move || {
                ElementSplitter::new(ReadAhead::spawn(move || decompress(file, &path)), "event")
            },
            |splitter, buffer| splitter.next_element_into(buffer),
            parse_event,
        );
        Self { pipeline }
    }

    pub fn read_next(&mut self) -> Option<(SimTime, Box<dyn EventTrait>)> {
        let parsed = self.pipeline.next()?;
        let event = parsed.event.unwrap_or_else(|attributes| {
            handle(&attributes, &CreateMissingIds).expect("Creating missing ids never fails.")
        });
        Some((parsed.time, event))
    }
}

fn decompress(file: File, path: &Path) -> Box<dyn BufRead> {
    match path.extension().unwrap().to_str() {
        Some("gz") => Box::new(BufReader::new(flate2::read::GzDecoder::new(file))),
        Some("zst") => Box::new(BufReader::new(
            ZstdDecoder::new(file).expect("Failed to create zstd decoder"),
        )),
        _ => Box::new(BufReader::new(file)),
    }
}

/// Parses a single `<event .../>` element. `index` is the position of the event in the file.
fn parse_event(index: usize, bytes: &[u8]) -> ParsedEvent {
    let attributes = parse_attributes(index, bytes);
    let time = SimTime::parse_decimal_seconds(value_from_name(&attributes, "time").unwrap())
        .unwrap_or_else(|e| panic!("Could not parse event time: {e}"));
    ParsedEvent {
        time,
        event: handle(&attributes, &ExistingIds).ok_or(attributes),
    }
}

fn parse_attributes(index: usize, bytes: &[u8]) -> Vec<OwnedAttribute> {
    // Parsing each element with the same parser as a whole document keeps, e.g., the handling of
    // entities and whitespace in attribute values.
    let mut parser = EventReader::new(bytes);
    loop {
        match parser.next() {
            Ok(XmlEvent::StartElement { attributes, .. }) => return attributes,
            Ok(XmlEvent::EndDocument) => panic!("Event number {index} contains no element."),
            Ok(_) => continue,
            Err(e) => panic!("Failed to parse event number {index}: {e}"),
        }
    }
}

fn handle(attr: &Vec<OwnedAttribute>, ids: &impl IdResolver) -> Option<Box<dyn EventTrait>> {
    let ev_type = &attr.get(1).unwrap().value;
    match ev_type.as_str() {
        ActivityEndEvent::TYPE => handle_act_end(attr, ids),
        PersonDepartureEvent::TYPE => handle_departure(attr, ids),
        TeleportationArrivalEvent::TYPE => travelled(attr, ids),
        PtTeleportationArrivalEvent::TYPE => handle_pt_travelled(attr, ids),
        PersonArrivalEvent::TYPE => handle_arrival(attr, ids),
        ActivityStartEvent::TYPE => handle_act_start(attr, ids),
        PersonEntersVehicleEvent::TYPE => handle_person_enters_veh(attr, ids),
        PersonLeavesVehicleEvent::TYPE => handle_person_leaves_veh(attr, ids),
        LinkEnterEvent::TYPE => handle_link_enter(attr, ids),
        LinkLeaveEvent::TYPE => handle_link_leave(attr, ids),
        VehicleEntersTrafficEvent::TYPE => handle_vehicle_enters_traffic(attr, ids),
        VehicleLeavesTrafficEvent::TYPE => handle_vehicle_leaves_traffic(attr, ids),
        PersonStuckEvent::TYPE => handle_person_stuck(attr, ids),
        _ => panic!("Unknown event type {ev_type}"),
    }
}

fn handle_vehicle_enters_traffic(
    attr: &Vec<OwnedAttribute>,
    ids: &impl IdResolver,
) -> Option<Box<dyn EventTrait>> {
    let time = SimTime::parse_decimal_seconds(value_from_name(attr, "time").unwrap()).unwrap();
    let relative_position: f64 = value_from_name(attr, "relativePosition")
        .unwrap()
        .parse()
        .unwrap();
    let person: Id<InternalPerson> = ids.resolve(value_from_name(attr, "person").unwrap())?;
    let link: Id<Link> = ids.resolve(value_from_name(attr, "link").unwrap())?;
    let vehicle: Id<InternalVehicle> = ids.resolve(value_from_name(attr, "vehicle").unwrap())?;
    let network_mode: Id<String> = ids.resolve(value_from_name(attr, "networkMode").unwrap())?;
    Some(Box::new(
        VehicleEntersTrafficEventBuilder::default()
            .time(time)
            .person(person)
            .link(link)
            .vehicle(vehicle)
            .network_mode(network_mode)
            .relative_position(relative_position)
            .build()
            .unwrap(),
    ))
}

fn handle_vehicle_leaves_traffic(
    attr: &Vec<OwnedAttribute>,
    ids: &impl IdResolver,
) -> Option<Box<dyn EventTrait>> {
    let time = SimTime::parse_decimal_seconds(value_from_name(attr, "time").unwrap()).unwrap();
    let relative_position: f64 = value_from_name(attr, "relativePosition")
        .unwrap()
        .parse()
        .unwrap();
    let person: Id<InternalPerson> = ids.resolve(value_from_name(attr, "person").unwrap())?;
    let link: Id<Link> = ids.resolve(value_from_name(attr, "link").unwrap())?;
    let vehicle: Id<InternalVehicle> = ids.resolve(value_from_name(attr, "vehicle").unwrap())?;
    let network_mode: Id<String> = ids.resolve(value_from_name(attr, "networkMode").unwrap())?;
    Some(Box::new(
        VehicleLeavesTrafficEventBuilder::default()
            .time(time)
            .person(person)
            .link(link)
            .vehicle(vehicle)
            .network_mode(network_mode)
            .relative_position(relative_position)
            .build()
            .unwrap(),
    ))
}

fn handle_act_end(
    attr: &Vec<OwnedAttribute>,
    ids: &impl IdResolver,
) -> Option<Box<dyn EventTrait>> {
    let time = SimTime::parse_decimal_seconds(value_from_name(attr, "time").unwrap()).unwrap();
    let x: f64 = value_from_name(attr, "x").unwrap().parse().unwrap();
    let y: f64 = value_from_name(attr, "y").unwrap().parse().unwrap();
    let person: Id<InternalPerson> = ids.resolve(value_from_name(attr, "person").unwrap())?;
    let link: Id<Link> = ids.resolve(value_from_name(attr, "link").unwrap())?;
    let act_type: Id<String> = ids.resolve(value_from_name(attr, "actType").unwrap())?;
    Some(Box::new(
        ActivityEndEventBuilder::default()
            .time(time)
            .person(person)
            .link(link)
            .act_type(act_type)
            .coordinate(Coordinate::new_2d(x, y))
            .build()
            .unwrap(),
    ))
}

fn handle_act_start(
    attr: &Vec<OwnedAttribute>,
    ids: &impl IdResolver,
) -> Option<Box<dyn EventTrait>> {
    let time = SimTime::parse_decimal_seconds(value_from_name(attr, "time").unwrap()).unwrap();
    let x: f64 = value_from_name(attr, "x").unwrap().parse().unwrap();
    let y: f64 = value_from_name(attr, "y").unwrap().parse().unwrap();
    let person: Id<InternalPerson> = ids.resolve(value_from_name(attr, "person").unwrap())?;
    let link: Id<Link> = ids.resolve(value_from_name(attr, "link").unwrap())?;
    let act_type: Id<String> = ids.resolve(value_from_name(attr, "actType").unwrap())?;
    Some(Box::new(
        ActivityStartEventBuilder::default()
            .time(time)
            .person(person)
            .link(link)
            .act_type(act_type)
            .coordinate(Coordinate::new_2d(x, y))
            .build()
            .unwrap(),
    ))
}

fn handle_departure(
    attr: &Vec<OwnedAttribute>,
    ids: &impl IdResolver,
) -> Option<Box<dyn EventTrait>> {
    let time = SimTime::parse_decimal_seconds(value_from_name(attr, "time").unwrap()).unwrap();
    let person: Id<InternalPerson> = ids.resolve(value_from_name(attr, "person").unwrap())?;
    let link: Id<Link> = ids.resolve(value_from_name(attr, "link").unwrap())?;
    let leg_mode: Id<String> = ids.resolve(value_from_name(attr, "legMode").unwrap())?;
    let routing_mode: Id<String> =
        ids.resolve(value_from_name(attr, "computationalRoutingMode").unwrap())?;
    Some(Box::new(
        PersonDepartureEventBuilder::default()
            .time(time)
            .person(person)
            .link(link)
            .leg_mode(leg_mode)
            .routing_mode(routing_mode)
            .build()
            .unwrap(),
    ))
}

fn handle_arrival(
    attr: &Vec<OwnedAttribute>,
    ids: &impl IdResolver,
) -> Option<Box<dyn EventTrait>> {
    let time = SimTime::parse_decimal_seconds(value_from_name(attr, "time").unwrap()).unwrap();
    let person: Id<InternalPerson> = ids.resolve(value_from_name(attr, "person").unwrap())?;
    let link: Id<Link> = ids.resolve(value_from_name(attr, "link").unwrap())?;
    let leg_mode: Id<String> = ids.resolve(value_from_name(attr, "legMode").unwrap())?;
    Some(Box::new(
        PersonArrivalEventBuilder::default()
            .time(time)
            .person(person)
            .link(link)
            .leg_mode(leg_mode)
            .build()
            .unwrap(),
    ))
}

fn travelled(attr: &Vec<OwnedAttribute>, ids: &impl IdResolver) -> Option<Box<dyn EventTrait>> {
    let time = SimTime::parse_decimal_seconds(value_from_name(attr, "time").unwrap()).unwrap();
    let person: Id<InternalPerson> = ids.resolve(value_from_name(attr, "person").unwrap())?;
    let distance: f64 = value_from_name(attr, "distance").unwrap().parse().unwrap();
    let mode: Id<String> = ids.resolve(value_from_name(attr, "mode").unwrap())?;
    Some(Box::new(
        TeleportationArrivalEventBuilder::default()
            .time(time)
            .person(person)
            .mode(mode)
            .distance(distance)
            .build()
            .unwrap(),
    ))
}

fn handle_pt_travelled(
    attr: &Vec<OwnedAttribute>,
    ids: &impl IdResolver,
) -> Option<Box<dyn EventTrait>> {
    let time = SimTime::parse_decimal_seconds(value_from_name(attr, "time").unwrap()).unwrap();
    let person: Id<InternalPerson> = ids.resolve(value_from_name(attr, "person").unwrap())?;
    let distance: f64 = value_from_name(attr, "distance").unwrap().parse().unwrap();
    let mode: Id<String> = ids.resolve(value_from_name(attr, "mode").unwrap())?;
    let line: Id<String> = ids.resolve(value_from_name(attr, "line").unwrap())?;
    let route: Id<String> = ids.resolve(value_from_name(attr, "route").unwrap())?;
    let boarding_time =
        SimTime::parse_decimal_seconds(value_from_name(attr, "boardingTime").unwrap()).unwrap();
    let access_fac: Id<String> = ids.resolve(value_from_name(attr, "accessFacility").unwrap())?;
    let egress_fac: Id<String> = ids.resolve(value_from_name(attr, "egressFacility").unwrap())?;
    Some(Box::new(
        PtTeleportationArrivalEventBuilder::default()
            .time(time)
            .person(person)
            .mode(mode)
            .distance(distance)
            .route(route)
            .line(line)
            .boarding_time(boarding_time)
            .access_facility(access_fac)
            .egress_facility(egress_fac)
            .build()
            .unwrap(),
    ))
}

fn handle_person_enters_veh(
    attr: &Vec<OwnedAttribute>,
    ids: &impl IdResolver,
) -> Option<Box<dyn EventTrait>> {
    let time = SimTime::parse_decimal_seconds(value_from_name(attr, "time").unwrap()).unwrap();
    let person: Id<InternalPerson> = ids.resolve(value_from_name(attr, "person").unwrap())?;
    let vehicle: Id<InternalVehicle> = ids.resolve(value_from_name(attr, "vehicle").unwrap())?;
    Some(Box::new(
        PersonEntersVehicleEventBuilder::default()
            .time(time)
            .person(person)
            .vehicle(vehicle)
            .build()
            .unwrap(),
    ))
}

fn handle_person_leaves_veh(
    attr: &Vec<OwnedAttribute>,
    ids: &impl IdResolver,
) -> Option<Box<dyn EventTrait>> {
    let time = SimTime::parse_decimal_seconds(value_from_name(attr, "time").unwrap()).unwrap();
    let person: Id<InternalPerson> = ids.resolve(value_from_name(attr, "person").unwrap())?;
    let vehicle: Id<InternalVehicle> = ids.resolve(value_from_name(attr, "vehicle").unwrap())?;
    Some(Box::new(
        PersonLeavesVehicleEventBuilder::default()
            .time(time)
            .person(person)
            .vehicle(vehicle)
            .build()
            .unwrap(),
    ))
}

fn handle_link_enter(
    attr: &Vec<OwnedAttribute>,
    ids: &impl IdResolver,
) -> Option<Box<dyn EventTrait>> {
    let time = SimTime::parse_decimal_seconds(value_from_name(attr, "time").unwrap()).unwrap();
    let link: Id<Link> = ids.resolve(value_from_name(attr, "link").unwrap())?;
    let vehicle: Id<InternalVehicle> = ids.resolve(value_from_name(attr, "vehicle").unwrap())?;
    Some(Box::new(
        LinkEnterEventBuilder::default()
            .time(time)
            .link(link)
            .vehicle(vehicle)
            .build()
            .unwrap(),
    ))
}

fn handle_link_leave(
    attr: &Vec<OwnedAttribute>,
    ids: &impl IdResolver,
) -> Option<Box<dyn EventTrait>> {
    let time = SimTime::parse_decimal_seconds(value_from_name(attr, "time").unwrap()).unwrap();
    let link: Id<Link> = ids.resolve(value_from_name(attr, "link").unwrap())?;
    let vehicle: Id<InternalVehicle> = ids.resolve(value_from_name(attr, "vehicle").unwrap())?;
    Some(Box::new(
        LinkLeaveEventBuilder::default()
            .time(time)
            .link(link)
            .vehicle(vehicle)
            .build()
            .unwrap(),
    ))
}

fn handle_person_stuck(
    attr: &Vec<OwnedAttribute>,
    ids: &impl IdResolver,
) -> Option<Box<dyn EventTrait>> {
    let time = SimTime::parse_decimal_seconds(value_from_name(attr, "time").unwrap()).unwrap();
    let person: Id<InternalPerson> = ids.resolve(value_from_name(attr, "person").unwrap())?;
    let link = match value_from_name(attr, "link") {
        Some(value) => Some(ids.resolve::<Link>(value)?),
        None => None,
    };
    let leg_mode = match value_from_name(attr, "legMode") {
        Some(value) => Some(ids.resolve::<String>(value)?),
        None => None,
    };
    let reason = value_from_name(attr, "reason").cloned();
    Some(Box::new(
        PersonStuckEventBuilder::default()
            .time(time)
            .person(person)
            .link(link)
            .leg_mode(leg_mode)
            .reason(reason)
            .build()
            .unwrap(),
    ))
}

fn value_from_name<'a>(attr: &'a Vec<OwnedAttribute>, name: &str) -> Option<&'a String> {
    attr.iter()
        .find(|&a| a.name.local_name.eq(name))
        .map(|a| &a.value)
}

#[cfg(test)]
mod tests {
    use super::{XmlEventsReader, XmlEventsWriter};
    use crate::simulation::events::{
        ActivityStartEvent, ActivityStartEventBuilder, EventTrait, PersonStuckEvent,
        PersonStuckEventBuilder,
    };
    use crate::simulation::id::Id;
    use crate::simulation::scenario::Coordinate;
    use crate::simulation::time::SimTime;
    use macros::deterministic_id_test;
    use std::fs;
    use std::io::Read;
    use std::path::Path;
    use std::path::PathBuf;

    #[deterministic_id_test]
    fn person_stuck_xml_round_trip_preserves_optional_attributes() {
        let output_dir = PathBuf::from("./test_output/io/xml_events/person_stuck_round_trip");
        fs::create_dir_all(&output_dir).unwrap();
        let path = output_dir.join("events.xml");
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

        assert_eq!(
            "<event time=\"42\" type=\"stuckAndAbort\" person=\"person-with-details\" link=\"link-1\" legMode=\"car\" reason=\"mobsim end\"/>\n",
            XmlEventsWriter::event_2_string(&with_optional_attributes)
        );
        assert_eq!(
            "<event time=\"42\" type=\"stuckAndAbort\" person=\"person-without-details\"/>\n",
            XmlEventsWriter::event_2_string(&without_optional_attributes)
        );

        let writer = XmlEventsWriter::new(&path);
        writer.on_any(&with_optional_attributes);
        writer.on_any(&without_optional_attributes);
        writer.finish();

        let mut reader = XmlEventsReader::new(&path);
        for expected in [&with_optional_attributes, &without_optional_attributes] {
            let (parsed_time, parsed_event) = reader.read_next().unwrap();
            assert_eq!(time, parsed_time);
            let parsed_event = parsed_event
                .as_any()
                .downcast_ref::<PersonStuckEvent>()
                .unwrap();
            assert_eq!(expected, parsed_event);
        }
    }

    #[deterministic_id_test]
    fn xml_event_round_trip_preserves_nanoseconds() {
        let output_dir = PathBuf::from("./test_output/io/xml_events/nanos_round_trip");
        fs::create_dir_all(&output_dir).unwrap();
        let path = output_dir.join("events.xml");

        let event: Box<dyn EventTrait> = Box::new(
            ActivityStartEventBuilder::default()
                .time(SimTime::from_nanos(42_123_456_789))
                .person(Id::create("person-1"))
                .link(Id::create("link-1"))
                .act_type(Id::create("home"))
                .coordinate(Coordinate::new_2d(1.0, 2.0))
                .build()
                .unwrap(),
        );

        let writer = XmlEventsWriter::new(&path);
        writer.on_any(event.as_ref());
        writer.finish();

        let mut reader = XmlEventsReader::new(&path);
        let (time, parsed_event) = reader.read_next().unwrap();

        assert_eq!(SimTime::from_nanos(42_123_456_789), time);

        let parsed_event = parsed_event
            .as_any()
            .downcast_ref::<ActivityStartEvent>()
            .unwrap();
        assert_eq!(Id::create("person-1"), parsed_event.person);
        assert_eq!(Id::create("link-1"), parsed_event.link);
        assert_eq!(Id::create("home"), parsed_event.act_type);
        assert_eq!(Coordinate::new_2d(1.0, 2.0), parsed_event.coordinate);
    }

    #[deterministic_id_test]
    fn zstd_xml_event_round_trip_preserves_nanoseconds() {
        let output_dir = PathBuf::from("./test_output/io/xml_events/zstd_nanos_round_trip");
        fs::create_dir_all(&output_dir).unwrap();
        let path = output_dir.join("events.xml.zst");

        let event: Box<dyn EventTrait> = Box::new(
            ActivityStartEventBuilder::default()
                .time(SimTime::from_nanos(42_123_456_789))
                .person(Id::create("person-1"))
                .link(Id::create("link-1"))
                .act_type(Id::create("home"))
                .coordinate(Coordinate::new_2d(1.0, 2.0))
                .build()
                .unwrap(),
        );

        let writer = XmlEventsWriter::new(&path);
        writer.on_any(event.as_ref());
        writer.finish();

        let mut reader = XmlEventsReader::new(&path);
        let (time, parsed_event) = reader.read_next().unwrap();

        assert_eq!(SimTime::from_nanos(42_123_456_789), time);

        let parsed_event = parsed_event
            .as_any()
            .downcast_ref::<ActivityStartEvent>()
            .unwrap();
        assert_eq!(Id::create("person-1"), parsed_event.person);
        assert_eq!(Id::create("link-1"), parsed_event.link);
        assert_eq!(Id::create("home"), parsed_event.act_type);
        assert_eq!(Coordinate::new_2d(1.0, 2.0), parsed_event.coordinate);
    }

    #[deterministic_id_test]
    fn gzip_xml_event_writer_finishes_compressed_stream() {
        assert_compressed_event_stream_finishes(
            PathBuf::from("./test_output/io/xml_events/gzip_finished/events.xml.gz"),
            read_gzip_to_string,
        );
    }

    #[deterministic_id_test]
    fn zstd_xml_event_writer_finishes_compressed_stream() {
        assert_compressed_event_stream_finishes(
            PathBuf::from("./test_output/io/xml_events/zstd_finished/events.xml.zst"),
            read_zstd_to_string,
        );
    }

    fn assert_compressed_event_stream_finishes(
        path: PathBuf,
        read_to_string: fn(&PathBuf) -> String,
    ) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();

        let event: Box<dyn EventTrait> = Box::new(
            ActivityStartEventBuilder::default()
                .time(SimTime::from_nanos(42_123_456_789))
                .person(Id::create("person-1"))
                .link(Id::create("link-1"))
                .act_type(Id::create("home"))
                .coordinate(Coordinate::new_2d(1.0, 2.0))
                .build()
                .unwrap(),
        );

        let writer = XmlEventsWriter::new(&path);
        writer.on_any(event.as_ref());
        writer.finish();
        writer.finish();

        let xml = read_to_string(&path);
        assert!(xml.contains("</events>"));

        let mut reader = XmlEventsReader::new(&path);
        let (time, _) = reader.read_next().unwrap();
        assert_eq!(SimTime::from_nanos(42_123_456_789), time);
    }

    fn read_gzip_to_string(path: &PathBuf) -> String {
        let file = fs::File::open(path).unwrap();
        let mut decoder = flate2::read::GzDecoder::new(file);
        let mut output = String::new();
        decoder.read_to_string(&mut output).unwrap();
        output
    }

    fn read_zstd_to_string(path: &PathBuf) -> String {
        let file = fs::File::open(path).unwrap();
        let mut decoder = zstd::stream::read::Decoder::new(file).unwrap();
        let mut output = String::new();
        decoder.read_to_string(&mut output).unwrap();
        output
    }

    fn read_with_reader(path: &Path) -> Vec<(SimTime, String)> {
        let mut reader = XmlEventsReader::new(path);
        let mut result = Vec::new();
        while let Some((time, event)) = reader.read_next() {
            result.push((time, XmlEventsWriter::event_2_string(event.as_ref())));
        }
        result
    }

    #[deterministic_id_test]
    fn reader_reads_prefixed_events() {
        let folder = PathBuf::from("./test_output/io/xml_events/reader_reads_prefixed_events");
        fs::create_dir_all(&folder).unwrap();
        let path = folder.join("events.xml");
        fs::write(
            &path,
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
             <e:events xmlns:e=\"http://www.matsim.org/events\" version=\"1.0\">\n\
             <e:event time=\"1\" type=\"travelled\" person=\"a\" distance=\"10\" mode=\"walk\"/>\n\
             </e:events>\n",
        )
        .unwrap();

        let expected = vec![(
            SimTime::from_secs(1),
            "<event time=\"1\" type=\"travelled\" person=\"a\" distance=\"10\" mode=\"walk\"/>\n"
                .to_string(),
        )];
        assert_eq!(expected, read_with_reader(&path));
    }

    #[deterministic_id_test]
    #[should_panic(expected = "Input ended before the end of the root element.")]
    fn events_file_ending_after_an_event_panics() {
        let folder =
            PathBuf::from("./test_output/io/xml_events/events_file_ending_after_an_event_panics");
        fs::create_dir_all(&folder).unwrap();
        let path = folder.join("events.xml");
        fs::write(
            &path,
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
             <events version=\"1.0\">\n\
             <event time=\"1\" type=\"travelled\" person=\"a\" distance=\"10\" mode=\"walk\"/>\n",
        )
        .unwrap();

        read_with_reader(&path);
    }

    #[deterministic_id_test]
    fn reader_unescapes_attribute_values() {
        let folder = PathBuf::from("./test_output/io/xml_events/reader_unescapes_attribute_values");
        fs::create_dir_all(&folder).unwrap();
        let path = folder.join("events.xml");
        fs::write(
            &path,
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
             <events version=\"1.0\">\n\
             <!-- <event time=\"0\" type=\"actend\"/> is a comment -->\n\
             <event time=\"1\" type=\"actend\" person=\"a &amp; b\" link=\"l&quot;1\" x=\"1\" y=\"2\" actType=\"home &lt;3&gt;\"/>\n\
             <event\n  time=\"2\" type=\"travelled\" person=\"c\" distance=\"10\" mode=\"walk\" />\n\
             <event time=\"3\" type=\"stuckAndAbort\" person=\"d\" link=\"l2\" legMode=\"car\" reason=\"first\nsecond &#65;\"/>\n\
             </events>\n",
        )
        .unwrap();

        let expected = [
            (1, "<event time=\"1\" type=\"actend\" person=\"a & b\" link=\"l\"1\" x=\"1\" y=\"2\" actType=\"home <3>\"/>\n"),
            (2, "<event time=\"2\" type=\"travelled\" person=\"c\" distance=\"10\" mode=\"walk\"/>\n"),
            (3, "<event time=\"3\" type=\"stuckAndAbort\" person=\"d\" link=\"l2\" legMode=\"car\" reason=\"first\nsecond A\"/>\n"),
        ]
        .map(|(secs, event)| (SimTime::from_secs(secs), event.to_string()));
        assert_eq!(expected.to_vec(), read_with_reader(&path));
    }
}
