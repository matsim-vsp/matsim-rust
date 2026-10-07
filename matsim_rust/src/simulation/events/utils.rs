use crate::generated::events::GenericEvent;
use crate::simulation::events::{EventTrait, EventsManager, GenericEventBuilder, comparison};
use crate::simulation::io::proto::proto_events::{ProtoEventsReader, process_events};
use crate::simulation::io::xml::events::{XmlEventsReader, XmlEventsWriter};
use crate::simulation::time::SimTime;
use std::error::Error;
use std::fmt;
use std::fmt::Display;
use std::fs::File;
use std::io::{Read, Seek};
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

struct StatefulProtoReader<R: Read + Seek> {
    reader: ProtoEventsReader<R>,
    preloaded_time_step: (SimTime, Vec<GenericEvent>),
}

impl StatefulProtoReader<File> {
    fn from_file(path: impl AsRef<Path>) -> Self {
        Self {
            reader: ProtoEventsReader::from_file(path.as_ref()),
            preloaded_time_step: (SimTime::default(), Vec::new()),
        }
    }
}

impl StatefulReader for StatefulProtoReader<File> {
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
        process_events(
            self.preloaded_time_step.0,
            &self.preloaded_time_step.1,
            manager,
        )
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
    let file_extension = path
        .as_ref()
        .extension()
        .ok_or_else(|| FileTypeError::NotGiven)?;

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
        readers.sort_by(|a, b| a.get_preloaded_time().cmp(&b.get_preloaded_time()));

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
            &PathBuf::from(&resource_folder),
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
}
