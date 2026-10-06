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
    AgentWaitingForPtEvent, AgentWaitingForPtEventBuilder, EventHandlerRegisterFn, EventTrait,
    EventsManager, GenericEvent, LinkEnterEvent, LinkEnterEventBuilder, LinkLeaveEvent,
    LinkLeaveEventBuilder, PersonArrivalEvent, PersonArrivalEventBuilder, PersonDepartureEvent,
    PersonDepartureEventBuilder, PersonEntersVehicleEvent, PersonEntersVehicleEventBuilder,
    PersonLeavesVehicleEvent, PersonLeavesVehicleEventBuilder, PersonStuckEvent,
    PersonStuckEventBuilder, PtTeleportationArrivalEvent, PtTeleportationArrivalEventBuilder,
    TeleportationArrivalEvent, TeleportationArrivalEventBuilder, TransitDriverStartsEvent,
    TransitDriverStartsEventBuilder, VehicleArrivesAtFacilityEvent,
    VehicleArrivesAtFacilityEventBuilder, VehicleDepartsAtFacilityEvent,
    VehicleDepartsAtFacilityEventBuilder, VehicleEntersTrafficEvent,
    VehicleEntersTrafficEventBuilder, VehicleLeavesTrafficEvent, VehicleLeavesTrafficEventBuilder,
};
use crate::simulation::id::Id;
use crate::simulation::scenario::Coordinate;
use crate::simulation::scenario::network::Link;
use crate::simulation::scenario::population::InternalPerson;
use crate::simulation::scenario::vehicles::InternalVehicle;
use crate::simulation::time::SimTime;

/// An event together with the simulation time at which it was read.
pub type TimedEvent = (SimTime, Box<dyn EventTrait>);

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
        } else if let Some(ev) = e.as_any().downcast_ref::<TransitDriverStartsEvent>() {
            format!(
                "<event time=\"{}\" type=\"{}\" driverId=\"{}\" vehicleId=\"{}\" transitLineId=\"{}\" transitRouteId=\"{}\" departureId=\"{}\"/>\n",
                ev.time().format_decimal_seconds(),
                ev.type_(),
                ev.driver,
                ev.vehicle,
                ev.line,
                ev.route,
                ev.departure
            )
        } else if let Some(ev) = e.as_any().downcast_ref::<VehicleArrivesAtFacilityEvent>() {
            format!(
                "<event time=\"{}\" type=\"{}\" vehicle=\"{}\" facility=\"{}\" delay=\"{:?}\"/>\n",
                ev.time().format_decimal_seconds(),
                ev.type_(),
                ev.vehicle,
                ev.facility,
                ev.delay
            )
        } else if let Some(ev) = e.as_any().downcast_ref::<VehicleDepartsAtFacilityEvent>() {
            format!(
                "<event time=\"{}\" type=\"{}\" vehicle=\"{}\" facility=\"{}\" delay=\"{:?}\"/>\n",
                ev.time().format_decimal_seconds(),
                ev.type_(),
                ev.vehicle,
                ev.facility,
                ev.delay
            )
        } else if let Some(ev) = e.as_any().downcast_ref::<AgentWaitingForPtEvent>() {
            format!(
                "<event time=\"{}\" type=\"{}\" person=\"{}\" agent=\"{}\" atStop=\"{}\" destinationStop=\"{}\"/>\n",
                ev.time().format_decimal_seconds(),
                ev.type_(),
                ev.person,
                ev.person,
                ev.at_stop,
                ev.destination_stop
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

pub struct XmlEventsReader {
    parser: EventReader<Box<dyn BufRead>>,
}

impl XmlEventsReader {
    pub fn new(events_file: impl AsRef<Path>) -> Self {
        let file = File::open(events_file.as_ref())
            .unwrap_or_else(|_| panic!("Could not open events file: {:?}", events_file.as_ref()));
        let buffered_reader: Box<dyn BufRead> =
            match events_file.as_ref().extension().unwrap().to_str() {
                Some("gz") => Box::new(BufReader::new(flate2::read::GzDecoder::new(file))),
                Some("zst") => Box::new(BufReader::new(
                    ZstdDecoder::new(file).expect("Failed to create zstd decoder"),
                )),
                _ => Box::new(BufReader::new(file)),
            };
        let parser = EventReader::new(buffered_reader);
        Self { parser }
    }
    pub fn read_next(&mut self) -> Option<(SimTime, Box<dyn EventTrait>)> {
        match self.try_read_next() {
            Ok(event) => event,
            Err(XmlEventsReadError::Xml(_)) => None,
            Err(XmlEventsReadError::InvalidEvent(error)) => {
                panic!("Could not parse event XML: {error}")
            }
        }
    }

    pub fn try_read_next(&mut self) -> Result<Option<TimedEvent>, XmlEventsReadError> {
        loop {
            let result = self.parser.next();
            match result {
                Ok(XmlEvent::StartElement {
                    name, attributes, ..
                }) => {
                    if name.local_name.eq("event") {
                        let time_value = value_from_name(&attributes, "time").ok_or_else(|| {
                            XmlEventsReadError::InvalidEvent("missing time attribute".into())
                        })?;
                        let time = SimTime::parse_decimal_seconds(time_value).map_err(|error| {
                            XmlEventsReadError::InvalidEvent(format!("invalid event time: {error}"))
                        })?;
                        let event = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            handle(attributes)
                        }))
                        .map_err(|panic| XmlEventsReadError::InvalidEvent(panic_message(panic)))?;
                        return Ok(Some((time, event)));
                    }
                }
                Ok(XmlEvent::EndDocument) => return Ok(None),
                Err(error) => return Err(XmlEventsReadError::Xml(error)),
                _ => {
                    continue;
                }
            }
        }
    }
}

#[derive(Debug)]
pub enum XmlEventsReadError {
    Xml(xml::reader::Error),
    InvalidEvent(String),
}

impl std::fmt::Display for XmlEventsReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Xml(error) => write!(f, "{error}"),
            Self::InvalidEvent(error) => f.write_str(error),
        }
    }
}

fn panic_message(panic: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = panic.downcast_ref::<String>() {
        message.clone()
    } else if let Some(message) = panic.downcast_ref::<&str>() {
        (*message).to_owned()
    } else {
        "invalid event attributes".into()
    }
}

fn handle(attr: Vec<OwnedAttribute>) -> Box<dyn EventTrait> {
    let ev_type = &attr.get(1).unwrap().value;
    match ev_type.as_str() {
        ActivityEndEvent::TYPE => handle_act_end(attr),
        PersonDepartureEvent::TYPE => handle_departure(attr),
        TeleportationArrivalEvent::TYPE => travelled(attr),
        PtTeleportationArrivalEvent::TYPE => handle_pt_travelled(attr),
        PersonArrivalEvent::TYPE => handle_arrival(attr),
        ActivityStartEvent::TYPE => handle_act_start(attr),
        PersonEntersVehicleEvent::TYPE => handle_person_enters_veh(attr),
        PersonLeavesVehicleEvent::TYPE => handle_person_leaves_veh(attr),
        LinkEnterEvent::TYPE => handle_link_enter(attr),
        LinkLeaveEvent::TYPE => handle_link_leave(attr),
        VehicleEntersTrafficEvent::TYPE => handle_vehicle_enters_traffic(attr),
        VehicleLeavesTrafficEvent::TYPE => handle_vehicle_leaves_traffic(attr),
        PersonStuckEvent::TYPE => handle_person_stuck(attr),
        TransitDriverStartsEvent::TYPE => handle_transit_driver_starts(attr),
        VehicleArrivesAtFacilityEvent::TYPE => handle_vehicle_arrives_at_facility(attr),
        VehicleDepartsAtFacilityEvent::TYPE => handle_vehicle_departs_at_facility(attr),
        AgentWaitingForPtEvent::TYPE => handle_waiting_for_pt(attr),
        _ => panic!("Unknown event type {ev_type}"),
    }
}

fn handle_vehicle_enters_traffic(attr: Vec<OwnedAttribute>) -> Box<dyn EventTrait> {
    let time = SimTime::parse_decimal_seconds(value_from_name(&attr, "time").unwrap()).unwrap();
    let relative_position: f64 = value_from_name(&attr, "relativePosition")
        .unwrap()
        .parse()
        .unwrap();
    let person: Id<InternalPerson> = Id::create(value_from_name(&attr, "person").unwrap());
    let link: Id<Link> = Id::create(value_from_name(&attr, "link").unwrap());
    let vehicle: Id<InternalVehicle> = Id::create(value_from_name(&attr, "vehicle").unwrap());
    let network_mode: Id<String> = Id::create(value_from_name(&attr, "networkMode").unwrap());
    Box::new(
        VehicleEntersTrafficEventBuilder::default()
            .time(time)
            .person(person)
            .link(link)
            .vehicle(vehicle)
            .network_mode(network_mode)
            .relative_position(relative_position)
            .build()
            .unwrap(),
    )
}

fn handle_vehicle_leaves_traffic(attr: Vec<OwnedAttribute>) -> Box<dyn EventTrait> {
    let time = SimTime::parse_decimal_seconds(value_from_name(&attr, "time").unwrap()).unwrap();
    let relative_position: f64 = value_from_name(&attr, "relativePosition")
        .unwrap()
        .parse()
        .unwrap();
    let person: Id<InternalPerson> = Id::create(value_from_name(&attr, "person").unwrap());
    let link: Id<Link> = Id::create(value_from_name(&attr, "link").unwrap());
    let vehicle: Id<InternalVehicle> = Id::create(value_from_name(&attr, "vehicle").unwrap());
    let network_mode: Id<String> = Id::create(value_from_name(&attr, "networkMode").unwrap());
    Box::new(
        VehicleLeavesTrafficEventBuilder::default()
            .time(time)
            .person(person)
            .link(link)
            .vehicle(vehicle)
            .network_mode(network_mode)
            .relative_position(relative_position)
            .build()
            .unwrap(),
    )
}

fn handle_transit_driver_starts(attr: Vec<OwnedAttribute>) -> Box<dyn EventTrait> {
    let time = SimTime::parse_decimal_seconds(value_from_name(&attr, "time").unwrap()).unwrap();
    Box::new(
        TransitDriverStartsEventBuilder::default()
            .time(time)
            .driver(Id::create(value_from_name(&attr, "driverId").unwrap()))
            .vehicle(Id::create(value_from_name(&attr, "vehicleId").unwrap()))
            .line(Id::create(value_from_name(&attr, "transitLineId").unwrap()))
            .route(Id::create(
                value_from_name(&attr, "transitRouteId").unwrap(),
            ))
            .departure(Id::create(value_from_name(&attr, "departureId").unwrap()))
            .build()
            .unwrap(),
    )
}

fn handle_vehicle_arrives_at_facility(attr: Vec<OwnedAttribute>) -> Box<dyn EventTrait> {
    let time = SimTime::parse_decimal_seconds(value_from_name(&attr, "time").unwrap()).unwrap();
    Box::new(
        VehicleArrivesAtFacilityEventBuilder::default()
            .time(time)
            .vehicle(Id::create(value_from_name(&attr, "vehicle").unwrap()))
            .facility(Id::create(value_from_name(&attr, "facility").unwrap()))
            .delay(value_from_name(&attr, "delay").unwrap().parse().unwrap())
            .build()
            .unwrap(),
    )
}

fn handle_vehicle_departs_at_facility(attr: Vec<OwnedAttribute>) -> Box<dyn EventTrait> {
    let time = SimTime::parse_decimal_seconds(value_from_name(&attr, "time").unwrap()).unwrap();
    Box::new(
        VehicleDepartsAtFacilityEventBuilder::default()
            .time(time)
            .vehicle(Id::create(value_from_name(&attr, "vehicle").unwrap()))
            .facility(Id::create(value_from_name(&attr, "facility").unwrap()))
            .delay(value_from_name(&attr, "delay").unwrap().parse().unwrap())
            .build()
            .unwrap(),
    )
}

fn handle_waiting_for_pt(attr: Vec<OwnedAttribute>) -> Box<dyn EventTrait> {
    let time = SimTime::parse_decimal_seconds(value_from_name(&attr, "time").unwrap()).unwrap();
    Box::new(
        AgentWaitingForPtEventBuilder::default()
            .time(time)
            // MATSim's own events carry both `agent` and `person`; protobuf keeps only `person`.
            // Accept either so one event file reads the same in both formats.
            .person(Id::create(
                value_from_name(&attr, "agent")
                    .or_else(|| value_from_name(&attr, "person"))
                    .unwrap(),
            ))
            .at_stop(Id::create(value_from_name(&attr, "atStop").unwrap()))
            .destination_stop(Id::create(
                value_from_name(&attr, "destinationStop").unwrap(),
            ))
            .build()
            .unwrap(),
    )
}

fn handle_act_end(attr: Vec<OwnedAttribute>) -> Box<dyn EventTrait> {
    let time = SimTime::parse_decimal_seconds(value_from_name(&attr, "time").unwrap()).unwrap();
    let x: f64 = value_from_name(&attr, "x").unwrap().parse().unwrap();
    let y: f64 = value_from_name(&attr, "y").unwrap().parse().unwrap();
    let person: Id<InternalPerson> = Id::create(value_from_name(&attr, "person").unwrap());
    let link: Id<Link> = Id::create(value_from_name(&attr, "link").unwrap());
    let act_type: Id<String> = Id::create(value_from_name(&attr, "actType").unwrap());
    Box::new(
        ActivityEndEventBuilder::default()
            .time(time)
            .person(person)
            .link(link)
            .act_type(act_type)
            .coordinate(Coordinate::new_2d(x, y))
            .build()
            .unwrap(),
    )
}

fn handle_act_start(attr: Vec<OwnedAttribute>) -> Box<dyn EventTrait> {
    let time = SimTime::parse_decimal_seconds(value_from_name(&attr, "time").unwrap()).unwrap();
    let x: f64 = value_from_name(&attr, "x").unwrap().parse().unwrap();
    let y: f64 = value_from_name(&attr, "y").unwrap().parse().unwrap();
    let person: Id<InternalPerson> = Id::create(value_from_name(&attr, "person").unwrap());
    let link: Id<Link> = Id::create(value_from_name(&attr, "link").unwrap());
    let act_type: Id<String> = Id::create(value_from_name(&attr, "actType").unwrap());
    Box::new(
        ActivityStartEventBuilder::default()
            .time(time)
            .person(person)
            .link(link)
            .act_type(act_type)
            .coordinate(Coordinate::new_2d(x, y))
            .build()
            .unwrap(),
    )
}

fn handle_departure(attr: Vec<OwnedAttribute>) -> Box<dyn EventTrait> {
    let time = SimTime::parse_decimal_seconds(value_from_name(&attr, "time").unwrap()).unwrap();
    let person: Id<InternalPerson> = Id::create(value_from_name(&attr, "person").unwrap());
    let link: Id<Link> = Id::create(value_from_name(&attr, "link").unwrap());
    let leg_mode: Id<String> = Id::create(value_from_name(&attr, "legMode").unwrap());
    let routing_mode: Id<String> =
        Id::create(value_from_name(&attr, "computationalRoutingMode").unwrap());
    Box::new(
        PersonDepartureEventBuilder::default()
            .time(time)
            .person(person)
            .link(link)
            .leg_mode(leg_mode)
            .routing_mode(routing_mode)
            .build()
            .unwrap(),
    )
}

fn handle_arrival(attr: Vec<OwnedAttribute>) -> Box<dyn EventTrait> {
    let time = SimTime::parse_decimal_seconds(value_from_name(&attr, "time").unwrap()).unwrap();
    let person: Id<InternalPerson> = Id::create(value_from_name(&attr, "person").unwrap());
    let link: Id<Link> = Id::create(value_from_name(&attr, "link").unwrap());
    let leg_mode: Id<String> = Id::create(value_from_name(&attr, "legMode").unwrap());
    Box::new(
        PersonArrivalEventBuilder::default()
            .time(time)
            .person(person)
            .link(link)
            .leg_mode(leg_mode)
            .build()
            .unwrap(),
    )
}

fn travelled(attr: Vec<OwnedAttribute>) -> Box<dyn EventTrait> {
    let time = SimTime::parse_decimal_seconds(value_from_name(&attr, "time").unwrap()).unwrap();
    let person: Id<InternalPerson> = Id::create(value_from_name(&attr, "person").unwrap());
    let distance: f64 = value_from_name(&attr, "distance").unwrap().parse().unwrap();
    let mode: Id<String> = Id::create(value_from_name(&attr, "mode").unwrap());
    Box::new(
        TeleportationArrivalEventBuilder::default()
            .time(time)
            .person(person)
            .mode(mode)
            .distance(distance)
            .build()
            .unwrap(),
    )
}

fn handle_pt_travelled(attr: Vec<OwnedAttribute>) -> Box<dyn EventTrait> {
    let time = SimTime::parse_decimal_seconds(value_from_name(&attr, "time").unwrap()).unwrap();
    let person: Id<InternalPerson> = Id::create(value_from_name(&attr, "person").unwrap());
    let distance: f64 = value_from_name(&attr, "distance").unwrap().parse().unwrap();
    let mode: Id<String> = Id::create(value_from_name(&attr, "mode").unwrap());
    let line: Id<String> = Id::create(value_from_name(&attr, "line").unwrap());
    let route: Id<String> = Id::create(value_from_name(&attr, "route").unwrap());
    let boarding_time =
        SimTime::parse_decimal_seconds(value_from_name(&attr, "boardingTime").unwrap()).unwrap();
    let access_fac: Id<String> = Id::create(value_from_name(&attr, "accessFacility").unwrap());
    let egress_fac: Id<String> = Id::create(value_from_name(&attr, "egressFacility").unwrap());
    Box::new(
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
    )
}

fn handle_person_enters_veh(attr: Vec<OwnedAttribute>) -> Box<dyn EventTrait> {
    let time = SimTime::parse_decimal_seconds(value_from_name(&attr, "time").unwrap()).unwrap();
    let person: Id<InternalPerson> = Id::create(value_from_name(&attr, "person").unwrap());
    let vehicle: Id<InternalVehicle> = Id::create(value_from_name(&attr, "vehicle").unwrap());
    Box::new(
        PersonEntersVehicleEventBuilder::default()
            .time(time)
            .person(person)
            .vehicle(vehicle)
            .build()
            .unwrap(),
    )
}

fn handle_person_leaves_veh(attr: Vec<OwnedAttribute>) -> Box<dyn EventTrait> {
    let time = SimTime::parse_decimal_seconds(value_from_name(&attr, "time").unwrap()).unwrap();
    let person: Id<InternalPerson> = Id::create(value_from_name(&attr, "person").unwrap());
    let vehicle: Id<InternalVehicle> = Id::create(value_from_name(&attr, "vehicle").unwrap());
    Box::new(
        PersonLeavesVehicleEventBuilder::default()
            .time(time)
            .person(person)
            .vehicle(vehicle)
            .build()
            .unwrap(),
    )
}

fn handle_link_enter(attr: Vec<OwnedAttribute>) -> Box<dyn EventTrait> {
    let time = SimTime::parse_decimal_seconds(value_from_name(&attr, "time").unwrap()).unwrap();
    let link: Id<Link> = Id::create(value_from_name(&attr, "link").unwrap());
    let vehicle: Id<InternalVehicle> = Id::create(value_from_name(&attr, "vehicle").unwrap());
    Box::new(
        LinkEnterEventBuilder::default()
            .time(time)
            .link(link)
            .vehicle(vehicle)
            .build()
            .unwrap(),
    )
}

fn handle_link_leave(attr: Vec<OwnedAttribute>) -> Box<dyn EventTrait> {
    let time = SimTime::parse_decimal_seconds(value_from_name(&attr, "time").unwrap()).unwrap();
    let link: Id<Link> = Id::create(value_from_name(&attr, "link").unwrap());
    let vehicle: Id<InternalVehicle> = Id::create(value_from_name(&attr, "vehicle").unwrap());
    Box::new(
        LinkLeaveEventBuilder::default()
            .time(time)
            .link(link)
            .vehicle(vehicle)
            .build()
            .unwrap(),
    )
}

fn handle_person_stuck(attr: Vec<OwnedAttribute>) -> Box<dyn EventTrait> {
    let time = SimTime::parse_decimal_seconds(value_from_name(&attr, "time").unwrap()).unwrap();
    let person: Id<InternalPerson> = Id::create(value_from_name(&attr, "person").unwrap());
    let link = value_from_name(&attr, "link").map(|value| Id::<Link>::create(value));
    let leg_mode = value_from_name(&attr, "legMode").map(|value| Id::<String>::create(value));
    let reason = value_from_name(&attr, "reason").cloned();
    Box::new(
        PersonStuckEventBuilder::default()
            .time(time)
            .person(person)
            .link(link)
            .leg_mode(leg_mode)
            .reason(reason)
            .build()
            .unwrap(),
    )
}

fn value_from_name<'a>(attr: &'a [OwnedAttribute], name: &str) -> Option<&'a String> {
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
    fn transit_events_read_java_output_and_round_trip() {
        use crate::simulation::events::{
            AgentWaitingForPtEvent, TransitDriverStartsEvent, VehicleArrivesAtFacilityEvent,
            VehicleDepartsAtFacilityEvent,
        };

        let output_dir = PathBuf::from("./test_output/io/xml_events/transit_round_trip");
        fs::create_dir_all(&output_dir).unwrap();
        let java = output_dir.join("java.xml");
        // Lines copied from a MATSim 14 pt-tutorial run.
        fs::write(
            &java,
            r#"<?xml version="1.0" encoding="utf-8"?>
<events version="1.0">
	<event time="21600.0" type="TransitDriverStarts" driverId="pt_tr_1_1" vehicleId="tr_1" transitLineId="Blue Line" transitRouteId="1to3" departureId="01"  />
	<event time="21801.0" type="VehicleArrivesAtFacility" vehicle="tr_1" facility="2a" delay="1.0"  />
	<event time="21801.0" type="VehicleDepartsAtFacility" vehicle="tr_1" facility="2a" delay="-39.0"  />
	<event time="25309.0" type="waitingForPt" person="280" agent="280" atStop="1" destinationStop="3"  />
</events>
"#,
        )
        .unwrap();

        let mut reader = XmlEventsReader::new(&java);
        let mut events = Vec::new();
        while let Some(event) = reader.read_next() {
            events.push(event);
        }
        assert_eq!(4, events.len());

        let starts = events[0]
            .1
            .as_any()
            .downcast_ref::<TransitDriverStartsEvent>()
            .unwrap();
        assert_eq!(SimTime::from_secs(21600), starts.time);
        assert_eq!("pt_tr_1_1", starts.driver.external());
        assert_eq!("tr_1", starts.vehicle.external());
        assert_eq!("Blue Line", starts.line.external());
        assert_eq!("1to3", starts.route.external());
        assert_eq!("01", starts.departure.external());
        let arrives = events[1]
            .1
            .as_any()
            .downcast_ref::<VehicleArrivesAtFacilityEvent>()
            .unwrap();
        assert_eq!(("2a", 1.0), (arrives.facility.external(), arrives.delay));
        let departs = events[2]
            .1
            .as_any()
            .downcast_ref::<VehicleDepartsAtFacilityEvent>()
            .unwrap();
        assert_eq!(("2a", -39.0), (departs.facility.external(), departs.delay));
        let waiting = events[3]
            .1
            .as_any()
            .downcast_ref::<AgentWaitingForPtEvent>()
            .unwrap();
        assert_eq!(
            ("280", "1", "3"),
            (
                waiting.person.external(),
                waiting.at_stop.external(),
                waiting.destination_stop.external()
            )
        );
        assert_eq!(
            "<event time=\"21801\" type=\"VehicleDepartsAtFacility\" vehicle=\"tr_1\" facility=\"2a\" delay=\"-39.0\"/>\n",
            XmlEventsWriter::event_2_string(departs)
        );

        let rust = output_dir.join("rust.xml");
        let writer = XmlEventsWriter::new(&rust);
        for (_, event) in &events {
            writer.on_any(event.as_ref());
        }
        writer.finish();
        let mut reader = XmlEventsReader::new(&rust);
        for (time, expected) in &events {
            let (parsed_time, parsed) = reader.read_next().unwrap();
            assert_eq!(*time, parsed_time);
            assert!(
                expected.as_ref() == parsed.as_ref(),
                "{expected:?} != {parsed:?}"
            );
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
}
