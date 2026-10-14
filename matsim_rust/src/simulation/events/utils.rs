use crate::simulation::events::{EventTrait, EventsManager, GenericEventBuilder, comparison};
use crate::simulation::io::proto::proto_events::{PreparedEvent, PreparedProtoEventsReader};
use crate::simulation::io::xml::events::{XmlEventsReader, XmlEventsWriter};
use crate::simulation::time::SimTime;
use std::error::Error;
use std::fmt;
use std::fmt::Display;
use std::path::{Path, PathBuf};
use tracing::info;

/// An event file reader with a state, containing the time and event data of the next time step.
/// This is needed so that multiple readers can be sorted by the time of their next event.
trait StatefulReader {
    /// preload the event time and event data of the next timestep into the reader state. Returns
    /// `true` if the next time step was successfully preloaded, or `false` if there are no more
    /// events to read.
    fn load_next(&mut self) -> bool;
    /// process the events that are currently preloaded in the state using the given event manager
    fn process_preloaded_events(&self, manager: &mut EventsManager);
    /// read the time of the preloaded events
    fn get_preloaded_time(&self) -> SimTime;
}

struct StatefulProtoReader {
    reader: PreparedProtoEventsReader,
    preloaded_time_step: (SimTime, Vec<PreparedEvent>),
}

impl StatefulProtoReader {
    fn from_file(path: impl AsRef<Path>) -> Self {
        Self {
            reader: PreparedProtoEventsReader::from_file(path.as_ref()),
            preloaded_time_step: (SimTime::default(), Vec::new()),
        }
    }
}

impl StatefulReader for StatefulProtoReader {
    fn load_next(&mut self) -> bool {
        match self.reader.next() {
            None => false,
            Some(time_step) => {
                self.preloaded_time_step = time_step;
                true
            }
        }
    }
    fn process_preloaded_events(&self, manager: &mut EventsManager) {
        // Pending events are converted here, so that missing ids are created in processing order.
        let (time, events) = &self.preloaded_time_step;
        for event in events {
            event.with_event(*time, |event| manager.process_event(event));
        }
    }

    fn get_preloaded_time(&self) -> SimTime {
        self.preloaded_time_step.0
    }
}

struct StatefulXmlReader {
    reader: XmlEventsReader,
    preloaded_event: (SimTime, Box<dyn EventTrait>),
}

impl StatefulXmlReader {
    fn from_file(path: impl AsRef<Path>) -> Self {
        Self {
            reader: XmlEventsReader::new(path),
            preloaded_event: (
                SimTime::default(),
                Box::new(
                    GenericEventBuilder::default()
                        .time(SimTime::default())
                        .build()
                        .unwrap(),
                ),
            ),
        }
    }
}

impl StatefulReader for StatefulXmlReader {
    fn load_next(&mut self) -> bool {
        match self.reader.read_next() {
            None => false,
            Some(next_event) => {
                self.preloaded_event = next_event;
                true
            }
        }
    }
    fn process_preloaded_events(&self, manager: &mut EventsManager) {
        manager.process_event(self.preloaded_event.1.as_ref());
    }

    fn get_preloaded_time(&self) -> SimTime {
        self.preloaded_event.0
    }
}

/// Error type for file types that are given to the event reading functions
#[derive(Debug)]
pub enum FileTypeError {
    Unimplemented(String),
    NotGiven,
    NotValidUnicode,
}

impl Display for FileTypeError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            FileTypeError::Unimplemented(ext) => write!(f, "unimplemented file type: {}", ext),
            FileTypeError::NotGiven => write!(f, "file was given without extension"),
            FileTypeError::NotValidUnicode => write!(f, "file extension is not valid unicode"),
        }
    }
}
impl Error for FileTypeError {}

/// Reads the events from the given file and publishes them to the given events manager.
/// When reading a proto file, assumes that ids are already loaded.
pub fn read_events(
    events_mgr: &mut EventsManager,
    path: impl AsRef<Path>,
) -> Result<(), FileTypeError> {
    info!("Reading events from file: {}", path.as_ref().display());
    let file_extension = path.as_ref().extension().ok_or(FileTypeError::NotGiven)?;

    let mut reader: Box<dyn StatefulReader> = match file_extension
        .to_str()
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("xml") | Some("gz") | Some("zst") => Box::new(StatefulXmlReader::from_file(path)),
        Some("binpb") | Some("pbf") => Box::new(StatefulProtoReader::from_file(path)),
        Some(other) => return Err(FileTypeError::Unimplemented(other.to_string())),
        None => return Err(FileTypeError::NotValidUnicode),
    };

    let mut last_reported_time_step = 0;

    // preload next events, and if they exist, process them
    while reader.load_next() {
        let secs = reader.get_preloaded_time().as_secs();
        let hour = secs / 3600;
        if hour > last_reported_time_step && secs.is_multiple_of(3600) {
            info!("Reading time step: {:?}h", hour);
            last_reported_time_step = hour;
        }

        // process the preloaded events
        reader.process_preloaded_events(events_mgr);
    }

    info!("Finished reading file.");
    events_mgr.finish();

    Ok(())
}

/// Reads all event files from the given folder with file name `{prefix}.{i}.{file_extension}`,
/// where `i=0..num_parts`, and publishes them to the given events manager.
/// When reading proto files, assumes that ids are already loaded.
pub fn read_partitioned_events(
    events_mgr: &mut EventsManager,
    folder: impl AsRef<Path>,
    prefix: &str,
    num_parts: u32,
    file_extension: &str,
) -> Result<(), FileTypeError> {
    let normalized_extension = file_extension.trim_start_matches('.').to_ascii_lowercase();

    let mut readers: Vec<Box<dyn StatefulReader>> = Vec::new();

    info!("Reading from Files: ");

    for i in 0..num_parts {
        let path =
            PathBuf::from(&folder.as_ref()).join(format!("{prefix}.{i}.{normalized_extension}"));
        info!("\t {}", path.display());

        // create stateful reader based on given file extension, return error if unsupported
        let mut reader: Box<dyn StatefulReader> = match normalized_extension.as_str() {
            "binpb" | "pbf" => Box::new(StatefulProtoReader::from_file(path)),
            "xml" | "xml.gz" | "xml.zst" => Box::new(StatefulXmlReader::from_file(path)),
            _ => return Err(FileTypeError::Unimplemented(normalized_extension)),
        };

        // initialize stateful reader by preloading the first state. If this returns None, the file
        // is empty so we don't add the reader to the readers.
        if !reader.load_next() {
            continue;
        }
        readers.push(reader);
    }

    info!("Starting to read files.");
    let mut last_reported_time_step = 0;
    while !readers.is_empty() {
        readers.sort_by_key(|a| a.get_preloaded_time());

        // get the reader with the smallest curr time step and process its events
        let reader = readers.first_mut().unwrap();

        let secs = reader.get_preloaded_time().as_secs();
        let hour = secs / 3600;
        if hour > last_reported_time_step && secs.is_multiple_of(3600) {
            info!("Reading time step: {:?}h", hour);
            last_reported_time_step = hour;
        }

        // process the events currently stored in "self.curr_time_step"
        reader.process_preloaded_events(events_mgr);

        if !reader.load_next() {
            readers.remove(0);
        };
    }
    info!("Finished reading files.");
    events_mgr.finish();

    Ok(())
}

/// Reads all proto events from the given folder and writes them to a single XML file (optionally
/// compressed as xml.gz, based on the file extension in the given output path).
/// Assumes that ids are already loaded.
pub fn convert_proto_to_xml_events(
    path_to_proto_files: impl AsRef<Path>,
    num_parts: u32,
    output_file_path: impl AsRef<Path> + 'static + Send + Clone,
) -> Result<(), FileTypeError> {
    let mut manager = EventsManager::new();

    let register_xml_writer = XmlEventsWriter::register_fn(output_file_path.clone());

    register_xml_writer(&mut manager);

    read_partitioned_events(
        &mut manager,
        path_to_proto_files.as_ref(),
        "events",
        num_parts,
        "binpb",
    )?;
    info!(
        "Finished writing to xml file ({}).",
        output_file_path.as_ref().to_str().unwrap()
    );
    Ok(())
}

#[derive(Debug, Clone)]
pub enum EventsFileNotEqualError {
    DifferentEventTimes,
    NotChronologicalOrder,
    DifferentNumberOfEvents,
    MissingEvent {
        event: String, // event in source 1 for which no identical event was found in source 2
    },
}

impl Display for EventsFileNotEqualError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EventsFileNotEqualError::DifferentEventTimes => write!(f, "Event times differ."),
            EventsFileNotEqualError::NotChronologicalOrder => {
                write!(f, "Events in both files are not in chronological order.")
            }
            EventsFileNotEqualError::DifferentNumberOfEvents => {
                write!(f, "Event sources have different numbers of events.")
            }
            EventsFileNotEqualError::MissingEvent { event } => write!(
                f,
                "No identical event found in source 2 for {event} in source 1."
            ),
        }
    }
}

/// Compares two XML, compressed XML, or protobuf event files using parallel reader threads.
pub fn compare_event_files(
    file1: impl AsRef<Path>,
    file2: impl AsRef<Path>,
) -> Result<(), EventsFileNotEqualError> {
    comparison::compare_event_files(file1.as_ref(), file2.as_ref())
}

/// Compares two folders containing partitioned `events.<rank>.<format>` files using parallel reader threads.
pub fn compare_event_folder(
    folder1: impl AsRef<Path>,
    folder2: impl AsRef<Path>,
) -> Result<(), EventsFileNotEqualError> {
    comparison::compare_event_folder(folder1.as_ref(), folder2.as_ref())
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::simulation::events::{EventHandlerRegisterFn, EventTrait};
    use crate::simulation::logging::init_std_out_logging_thread_local;
    use macros::deterministic_id_test;
    use std::rc::Rc;
    use std::sync::{Arc, Mutex};

    /// event handler that writes any event as a string (corresponding to an entry in an XML file)
    /// into a given vector
    struct EventsToVecCollector;

    impl EventsToVecCollector {
        /// on any event, push the event string into the vector
        fn on_any(&self, e: &dyn EventTrait, event_string_collection: Arc<Mutex<Vec<String>>>) {
            event_string_collection
                .lock()
                .unwrap()
                .push(XmlEventsWriter::event_2_string(e));
        }

        /// register the handler to the manager, telling it to call the above function on any event
        pub fn register_fn(
            event_string_collection: Arc<Mutex<Vec<String>>>,
        ) -> Box<EventHandlerRegisterFn> {
            Box::new(move |events: &mut EventsManager| {
                let to_vec_collector = Rc::new(EventsToVecCollector);

                events.on_any(move |e| {
                    to_vec_collector.on_any(e, event_string_collection.clone());
                });
            })
        }
    }

    /// returns a vector with the event strings (as used in XML files) expected to be written from
    /// the EventsToVecCollector when reading the files
    /// `/tests/resources/events/expected_events.0.xml`,
    /// `/tests/resources/events/expected_events.0.xml.gz` and
    /// `/tests/resources/events/events.0.binpb`.
    ///
    /// When reading the files `.../expected_events.0.xml` and `.../expected_events.1.xml` together
    /// (with `read_partitioned_events`) it is expected that an additional line appears between
    /// lines 2 and 3
    fn get_expected_event_strings_single_file() -> Vec<String> {
        [
            "<event time=\"32400\" type=\"actend\" person=\"100\" link=\"link1\" x=\"5\" y=\"10\" actType=\"home\"/>\n",
            "<event time=\"32400.5\" type=\"departure\" person=\"100\" link=\"link1\" legMode=\"walk\" computationalRoutingMode=\"car\"/>\n",
            "<event time=\"32408\" type=\"travelled\" person=\"100\" distance=\"10\" mode=\"walk\"/>\n",
            "<event time=\"32408\" type=\"arrival\" person=\"100\" link=\"link1\" legMode=\"walk\"/>\n",
            "<event time=\"32409\" type=\"actstart\" person=\"100\" link=\"link1\" x=\"5\" y=\"0\" actType=\"car interaction\"/>\n",
            "<event time=\"32409\" type=\"actend\" person=\"100\" link=\"link1\" x=\"5\" y=\"0\" actType=\"car interaction\"/>\n",
            "<event time=\"32409\" type=\"departure\" person=\"100\" link=\"link1\" legMode=\"car\" computationalRoutingMode=\"car\"/>\n",
            "<event time=\"32409\" type=\"PersonEntersVehicle\" person=\"100\" vehicle=\"100_car\"/>\n",
            "<event time=\"32409.123456789\" type=\"vehicle enters traffic\" person=\"100\" link=\"link1\" vehicle=\"100_car\" networkMode=\"car\" relativePosition=\"1\"/>\n",
            "<event time=\"32410\" type=\"left link\" link=\"link1\" vehicle=\"100_car\"/>\n",
            "<event time=\"32410\" type=\"entered link\" link=\"link2\" vehicle=\"100_car\"/>\n",
            "<event time=\"32511\" type=\"left link\" link=\"link2\" vehicle=\"100_car\"/>\n",
            "<event time=\"32511\" type=\"entered link\" link=\"link3\" vehicle=\"100_car\"/>\n",
            "<event time=\"32521\" type=\"vehicle leaves traffic\" person=\"100\" link=\"link3\" vehicle=\"100_car\" networkMode=\"car\" relativePosition=\"1\"/>\n",
            "<event time=\"32521\" type=\"PersonLeavesVehicle\" person=\"100\" vehicle=\"100_car\"/>\n",
            "<event time=\"32521\" type=\"arrival\" person=\"100\" link=\"link3\" legMode=\"car\"/>\n",
            "<event time=\"32522\" type=\"actstart\" person=\"100\" link=\"link3\" x=\"1100\" y=\"0\" actType=\"car interaction\"/>\n",
            "<event time=\"32522\" type=\"actend\" person=\"100\" link=\"link3\" x=\"1100\" y=\"0\" actType=\"car interaction\"/>\n",
            "<event time=\"32522\" type=\"departure\" person=\"100\" link=\"link3\" legMode=\"walk\" computationalRoutingMode=\"car\"/>\n",
            "<event time=\"32538\" type=\"travelled\" person=\"100\" distance=\"20\" mode=\"walk\"/>\n",
            "<event time=\"32538\" type=\"arrival\" person=\"100\" link=\"link3\" legMode=\"walk\"/>\n",
            "<event time=\"32539\" type=\"actstart\" person=\"100\" link=\"link3\" x=\"1100\" y=\"20\" actType=\"errands\"/>\n",
        ].iter().map(|s| s.to_string()).collect()
    }

    /// test the read_events function on a single xml file. Publishes the read events to an event
    /// manager where the above `EventsToVecCollector` is registered, and then compares the
    /// collected event strings with the expected event strings.
    #[deterministic_id_test]
    fn test_read_single_xml_file() {
        let _guard = init_std_out_logging_thread_local();
        let resource_folder = "./tests/resources/events/".to_string();
        let path = PathBuf::from(resource_folder).join("expected_events.0.xml");

        let mut events_mgr = EventsManager::new();

        // the event strings read from the XML file will be collected in this vector, to be compared
        // with the expected event strings
        let event_string_collection = Arc::new(Mutex::new(Vec::new()));

        // XmlEventsVecCollector is an event handler that writes event strings, like those written
        // into XML, into a given vector.
        let register_xml_event_collector =
            EventsToVecCollector::register_fn(event_string_collection.clone());

        register_xml_event_collector(&mut events_mgr);

        // read the XML events and publish them to the events manager, which will trigger the event
        // handler above and fill the event_string_collection vector
        read_events(&mut events_mgr, &path).unwrap();

        // assert that the event strings that the events handler handled are all the events inside
        // the read file "expected_events.0.xml"
        assert_eq!(
            event_string_collection.lock().unwrap().clone(),
            get_expected_event_strings_single_file()
        );
    }

    /// test the read_events function on a single xml.gz file. Publishes the read events to an event
    /// manager where the above `EventsToVecCollector` is registered, and then compares the
    /// collected event strings with the expected event strings.
    #[deterministic_id_test]
    fn test_read_single_xml_gz_file() {
        let _guard = init_std_out_logging_thread_local();
        let resource_folder = "./tests/resources/events/".to_string();
        let path = PathBuf::from(resource_folder).join("expected_events.0.xml.gz");

        let mut events_mgr = EventsManager::new();

        // the event strings read from the xml.gz file will be collected in this vector, to be
        // compared with the expected event strings
        let event_string_collection = Arc::new(Mutex::new(Vec::new()));

        // XmlEventsVecCollector is an event handler that writes event strings, like those written
        // into XML, into a given vector.
        let register_xml_event_collector =
            EventsToVecCollector::register_fn(event_string_collection.clone());

        register_xml_event_collector(&mut events_mgr);

        // read the events from the xml.gz file and publish them to the events manager, which will
        // trigger the event handler above and fill the event_string_collection vector
        read_events(&mut events_mgr, &path).unwrap();

        // assert that the event strings that the events handler handled are all the events inside
        // the read file "expected_events.0.xml.gz"
        assert_eq!(
            event_string_collection.lock().unwrap().clone(),
            get_expected_event_strings_single_file()
        );
    }

    /// test the read_events function on a single proto file. Publishes the read events to an event
    /// manager where the above `EventsToVecCollector` is registered, and then compares the
    /// collected event strings with the expected event strings.
    #[deterministic_id_test]
    #[ignore]
    // this test is ignored because once the proto definition changes, this test fails. Proto file
    // should be written during the test and read again. paul, jul '26.
    fn test_read_single_proto_file() {
        let _guard = init_std_out_logging_thread_local();
        let resource_folder = "./tests/resources/events/".to_string();
        let path = PathBuf::from(resource_folder).join("events.0.binpb");

        let mut events_mgr = EventsManager::new();

        // the event strings read from the proto file will be collected in this vector, to be
        // compared with the expected event strings
        let event_string_collection = Arc::new(Mutex::new(Vec::new()));

        // XmlEventsVecCollector is an event handler that writes event strings, like those written
        // into XML, into a given vector.
        let register_xml_event_collector =
            EventsToVecCollector::register_fn(event_string_collection.clone());

        register_xml_event_collector(&mut events_mgr);

        // read the proto events and publish them to the events manager, which will trigger the
        // event handler above and fill the event_string_collection vector
        read_events(&mut events_mgr, &path).unwrap();

        // assert that the event strings that the events handler handled are all the events inside
        // the read file "expected_events.0.binpb"
        // while we cannot inspect that file manually in a text editor, expected_events.0.binpb
        // contains the same events as expected_events.xml; as used in tests/io/events.rs as well
        assert_eq!(
            event_string_collection.lock().unwrap().clone(),
            get_expected_event_strings_single_file()
        );
    }

    /// test the `read_partitioned_events` function to check that the events read from two XML
    /// files are correctly published to the events manager, in the right order.
    /// Writes the corresponding XML string of all published events into a vector (using the above
    /// defined `EventsToVecCollector` event handler), and then comparing the vector with the
    /// expected event strings (corresponding to the events in the read XML files).
    #[deterministic_id_test]
    fn test_read_partitioned_xml() {
        let _guard = init_std_out_logging_thread_local();
        let resource_folder = "./tests/resources/events/".to_string();
        let num_parts = 2;

        let mut events_mgr = EventsManager::new();

        // the event strings read from the XML file will be collected in this vector, to be compared
        // with the expected event strings
        let event_string_collection = Arc::new(Mutex::new(Vec::new()));

        // XmlEventsVecCollector is an event handler that writes event strings, like those written
        // into XML, into a given vector.
        let register_xml_event_collector =
            EventsToVecCollector::register_fn(event_string_collection.clone());

        register_xml_event_collector(&mut events_mgr);

        // read the XML events and publish them to the events manager, which will trigger the event
        // handler above and fill the event_string_collection vector
        read_partitioned_events(
            &mut events_mgr,
            PathBuf::from(&resource_folder),
            "expected_events",
            num_parts,
            "xml",
        )
        .unwrap();

        let mut expected_string_collection = get_expected_event_strings_single_file();

        // add one line between lines 2 and 3, which is the event in the file
        // "expected_events.1.xml" that is not in "expected_events.0.xml".
        // Note that the new line has time 32406, which is between the times of the events in lines
        // 2 and 3 (32400.5 and 32408).
        expected_string_collection.insert(2, "<event time=\"32406\" type=\"travelled\" person=\"100\" distance=\"10\" mode=\"walk\"/>\n".to_string() );

        // assert that the event strings that the events handler handled are all the events inside
        // the read files "expected_events.0.xml" and "expected_events.1.xml", in correct order.
        assert_eq!(
            event_string_collection.lock().unwrap().clone(),
            expected_string_collection
        );
    }

    use crate::simulation::events::{
        ActivityEndEventBuilder, LinkEnterEventBuilder, PersonDepartureEventBuilder,
        PersonStuckEventBuilder, PtTeleportationArrivalEventBuilder,
        TeleportationArrivalEventBuilder,
    };
    use crate::simulation::id;
    use crate::simulation::id::Id;
    use crate::simulation::io::proto::proto_events::ProtoEventsWriter;
    use crate::simulation::scenario::Coordinate;
    use std::collections::BTreeMap;

    const SYNTHETIC_PARTS: u32 = 3;

    /// Events of one partition. Partitions have events at partly the same and partly different
    /// times, and new ids appear throughout the file, so that both the merge order and the order
    /// of id creation are observable.
    fn synthetic_events(part: u32) -> Vec<Box<dyn EventTrait>> {
        let mut events: Vec<Box<dyn EventTrait>> = Vec::new();
        for step in 0..300u32 {
            if (step + part) % 4 == 0 {
                // No events of this partition at this time.
                continue;
            }
            let time = SimTime::from_secs(u64::from(step * 3 + part % 2));
            let person = format!("person_{}_{}", part, step % 17);
            let link = format!("link_{}", (step * 7 + part) % 41);
            let mode = format!("mode_{}", step % 11);
            events.push(Box::new(
                ActivityEndEventBuilder::default()
                    .time(time)
                    .person(Id::create(&person))
                    .link(Id::create(&link))
                    .act_type(Id::create(&format!("act_{}", step % 5)))
                    .coordinate(Coordinate::new_2d(f64::from(step), 0.5))
                    .build()
                    .unwrap(),
            ));
            events.push(Box::new(
                PersonDepartureEventBuilder::default()
                    .time(time)
                    .person(Id::create(&person))
                    .link(Id::create(&link))
                    .leg_mode(Id::create(&mode))
                    .routing_mode(Id::create(&format!("routing_{}", step % 3)))
                    .build()
                    .unwrap(),
            ));
            events.push(Box::new(
                LinkEnterEventBuilder::default()
                    .time(time)
                    .link(Id::create(&format!("link_{}", step % 53)))
                    .vehicle(Id::create(&format!("{person}_{mode}")))
                    .build()
                    .unwrap(),
            ));
            if step % 5 == 0 {
                events.push(Box::new(
                    TeleportationArrivalEventBuilder::default()
                        .time(time)
                        .person(Id::create(&person))
                        .mode(Id::create(&mode))
                        .distance(f64::from(step) * 1.5)
                        .build()
                        .unwrap(),
                ));
                events.push(Box::new(
                    PtTeleportationArrivalEventBuilder::default()
                        .time(time)
                        .person(Id::create(&person))
                        .distance(12.25)
                        .mode(Id::create("pt"))
                        .route(Id::create(&format!("route_{}", step % 7)))
                        .line(Id::create(&format!("line_{}", step % 4)))
                        .boarding_time(time)
                        .access_facility(Id::create(&format!("stop_{}", step % 9)))
                        .egress_facility(Id::create(&format!("stop_{}", step % 10)))
                        .build()
                        .unwrap(),
                ));
            }
            if step % 37 == 0 {
                let mut stuck = PersonStuckEventBuilder::default();
                stuck.time(time).person(Id::create(&person));
                if step % 2 == 0 {
                    stuck
                        .link(Some(Id::create(&link)))
                        .leg_mode(Some(Id::create(&mode)));
                }
                events.push(Box::new(stuck.build().unwrap()));
            }
        }
        events
    }

    /// Writes the synthetic events of all partitions as `events.{part}.{extension}` and stores the
    /// created ids as `ids.binpb`. Returns the number of events.
    fn write_synthetic_events(folder: &Path, extension: &str) -> usize {
        std::fs::create_dir_all(folder).unwrap();
        let mut num_events = 0;
        for part in 0..SYNTHETIC_PARTS {
            let path = folder.join(format!("events.{part}.{extension}"));
            let mut manager = EventsManager::new();
            if extension == "binpb" {
                ProtoEventsWriter::register_fn(path)(&mut manager);
            } else {
                XmlEventsWriter::register_fn(path)(&mut manager);
            }
            for event in synthetic_events(part) {
                manager.process_event(event.as_ref());
                num_events += 1;
            }
            manager.finish();
        }
        id::store_to_file(&folder.join("ids.binpb"));
        num_events
    }

    type Recorded = (Vec<String>, BTreeMap<u64, Vec<String>>);

    /// Reads with `read` and records the processed events and the resulting id store.
    fn record(ids: Option<&Path>, read: &dyn Fn(&mut EventsManager)) -> Recorded {
        id::reset_store();
        if let Some(ids) = ids {
            id::load_from_file(ids);
        }
        let collected = Arc::new(Mutex::new(Vec::new()));
        let mut manager = EventsManager::new();
        EventsToVecCollector::register_fn(collected.clone())(&mut manager);
        read(&mut manager);
        let events = collected.lock().unwrap().clone();
        (events, id::snapshot_store())
    }

    /// Reads with `read` once on the main thread, where the pipelines convert in the global rayon
    /// pool, and once inside a rayon pool, where the pipelines convert on their own thread. Both
    /// must process the same events and create the same ids.
    fn assert_independent_of_threads(
        num_events: usize,
        ids: Option<&Path>,
        read: impl Fn(&mut EventsManager) + Sync,
    ) {
        let in_global_pool = record(ids, &read);
        assert_eq!(num_events, in_global_pool.0.len());

        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap();
        let in_pool = pool.install(|| record(ids, &read));
        assert_eq!(in_global_pool, in_pool);
    }

    fn assert_partitioned_reading_is_independent_of_threads(extension: &str) {
        let folder = PathBuf::from(format!(
            "./test_output/simulation/events/utils/partitioned_reading_is_independent_of_threads/{extension}"
        ));
        let num_events = write_synthetic_events(&folder, extension);
        let ids = folder.join("ids.binpb");
        let read_all = |manager: &mut EventsManager| {
            read_partitioned_events(manager, &folder, "events", SYNTHETIC_PARTS, extension).unwrap()
        };
        let first = folder.join(format!("events.0.{extension}"));
        let read_first = |manager: &mut EventsManager| read_events(manager, &first).unwrap();
        let num_events_first = synthetic_events(0).len();

        for ids in [None, Some(ids.as_path())] {
            assert_independent_of_threads(num_events, ids, read_all);
            assert_independent_of_threads(num_events_first, ids, read_first);
        }
    }

    #[deterministic_id_test]
    fn partitioned_xml_reading_is_independent_of_threads() {
        assert_partitioned_reading_is_independent_of_threads("xml.gz");
    }

    #[deterministic_id_test]
    fn partitioned_proto_reading_is_independent_of_threads() {
        assert_partitioned_reading_is_independent_of_threads("binpb");
    }

    #[deterministic_id_test]
    fn files_without_events_are_read() {
        let folder = PathBuf::from("./test_output/simulation/events/utils/files_without_events");
        std::fs::create_dir_all(&folder).unwrap();
        for extension in ["xml.gz", "binpb"] {
            let path = folder.join(format!("events.0.{extension}"));
            let mut manager = EventsManager::new();
            if extension == "binpb" {
                ProtoEventsWriter::register_fn(path.clone())(&mut manager);
            } else {
                XmlEventsWriter::register_fn(path.clone())(&mut manager);
            }
            manager.finish();

            let (events, _) = record(None, &|manager| read_events(manager, &path).unwrap());
            assert!(events.is_empty(), "{extension}");
        }
    }

    #[deterministic_id_test]
    #[should_panic(expected = "Failed to read input")]
    fn truncated_gz_file_panics() {
        let folder = PathBuf::from("./test_output/simulation/events/utils/truncated_gz_file");
        write_synthetic_events(&folder, "xml.gz");
        let path = folder.join("events.0.xml.gz");
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.truncate(bytes.len() / 2);
        std::fs::write(&path, bytes).unwrap();

        let mut manager = EventsManager::new();
        read_events(&mut manager, &path).unwrap();
    }

    #[deterministic_id_test]
    #[should_panic(expected = "Failed to parse event number 1")]
    fn invalid_event_element_panics() {
        let folder = PathBuf::from("./test_output/simulation/events/utils/invalid_event_element");
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join("events.xml");
        std::fs::write(
            &path,
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<events version=\"1.0\">\n\
             <event time=\"1\" type=\"travelled\" person=\"a\" distance=\"1\" mode=\"walk\"/>\n\
             <event time=\"2\" type=\"travelled\" person=a distance=\"1\" mode=\"walk\"/>\n\
             </events>\n",
        )
        .unwrap();

        let mut manager = EventsManager::new();
        read_events(&mut manager, &path).unwrap();
    }

    #[deterministic_id_test]
    #[should_panic(expected = "Failed to read delimited buffer")]
    fn truncated_proto_file_panics() {
        let folder = PathBuf::from("./test_output/simulation/events/utils/truncated_proto_file");
        write_synthetic_events(&folder, "binpb");
        let path = folder.join("events.0.binpb");
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.truncate(bytes.len() - 3);
        std::fs::write(&path, bytes).unwrap();

        let mut manager = EventsManager::new();
        read_events(&mut manager, &path).unwrap();
    }
}
