use macros::deterministic_id_test;
use matsim_rust::simulation::events::EventsManager;
use matsim_rust::simulation::events::utils;
use matsim_rust::simulation::events::utils::convert_proto_to_xml_events;
use matsim_rust::simulation::io::proto::proto_events::ProtoEventsWriter;
use matsim_rust::simulation::io::xml::events::XmlEventsReader;
use std::fs;
use std::path::PathBuf;

#[deterministic_id_test(matsim_rust)]
fn proto_events_convert_to_matching_xml() {
    let resource_folder = "./tests/resources/events/".to_string();
    let output_folder = "./test_output/io/xml_events/".to_string();
    let proto_folder = PathBuf::from(&output_folder).join("proto");
    let num_parts = 1;

    fs::create_dir_all(&proto_folder).unwrap();
    fs::create_dir_all(&output_folder).unwrap();

    // xml -> proto
    let expected_file = PathBuf::from(&resource_folder).join("expected_events.xml");
    let proto_file = proto_folder.join("events.0.binpb");
    let mut reader = XmlEventsReader::new(&expected_file);
    let mut events = EventsManager::new();
    let register_proto_writer = ProtoEventsWriter::register_fn(proto_file.clone());
    register_proto_writer(&mut events);

    while let Some((_time, event)) = reader.read_next() {
        events.process_event(event.as_ref());
    }
    events.finish();

    // proto -> xml
    convert_proto_to_xml_events(
        &proto_folder,
        num_parts,
        PathBuf::from(&output_folder).join("events.xml.gz"),
    )
    .expect("Failed to convert proto events to xml");

    // assert origin xml = new xml
    let generated_file = PathBuf::from(&output_folder).join("events.xml.gz");

    match utils::compare_event_files(generated_file, expected_file) {
        Ok(()) => (),
        Err(e) => panic!(
            "Generated XML event file ('file 1') and expected event file ('file 2') differ: {e}"
        ),
    }
}
