use crate::generated::events::GenericEvent;
use crate::simulation::events::EventTrait;
use crate::simulation::events::utils::EventsFileNotEqualError;
use crate::simulation::io::proto::proto_events::{ProtoEventsReader, event_to_proto};
use crate::simulation::io::xml::events::XmlEventsReader;
use crate::simulation::logging::init_std_out_logging_thread_local;
use crate::simulation::time::SimTime;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::thread::{self, JoinHandle};
use tracing::{error, info};

fn spawn_event_reader(
    source: EventSource,
    published_batch: Arc<Mutex<PublishedEventBatch>>,
    should_stop: Arc<AtomicBool>,
    barrier: Arc<Barrier>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let _guard = init_std_out_logging_thread_local();
        let mut reader = source.into_reader();

        loop {
            if should_stop.load(Ordering::Relaxed) {
                break;
            }

            let batch = reader.next_batch();
            let finished = batch.is_none();
            publish_batch(&published_batch, batch, finished);

            barrier.wait();
            barrier.wait();

            if finished || should_stop.load(Ordering::Relaxed) {
                break;
            }
        }
    })
}

fn compare_published_batches(
    batch1: Arc<Mutex<PublishedEventBatch>>,
    batch2: Arc<Mutex<PublishedEventBatch>>,
    barrier: Arc<Barrier>,
    should_stop: Arc<AtomicBool>,
    comparison_result: Arc<Mutex<Result<(), EventsFileNotEqualError>>>,
    source1: PathBuf,
    source2: PathBuf,
) {
    let mut event_count = 0_u64;
    let mut next_status_event = Some(1_u64);
    let mut last_time1 = None;
    let mut last_time2 = None;

    loop {
        barrier.wait();

        let (batch1, finished1) = take_published_batch(&batch1);
        let (batch2, finished2) = take_published_batch(&batch2);

        if finished1 && finished2 {
            barrier.wait();
            break;
        }

        if finished1 != finished2 {
            error!(
                "Event sources have different numbers of events: {} and {}",
                source1.display(),
                source2.display()
            );
            stop_with_error(
                &comparison_result,
                &should_stop,
                EventsFileNotEqualError::DifferentNumberOfEvents,
            );
            barrier.wait();
            break;
        }

        let (time1, events1, time2, events2) = match (batch1, batch2) {
            (Some((time1, events1)), Some((time2, events2))) => (time1, events1, time2, events2),
            _ => unreachable!("unfinished event reader did not publish a batch"),
        };

        if last_time1.is_some_and(|last| time1 < last)
            || last_time2.is_some_and(|last| time2 < last)
        {
            error!(
                "Events are not in chronological order in {} or {}",
                source1.display(),
                source2.display()
            );
            stop_with_error(
                &comparison_result,
                &should_stop,
                EventsFileNotEqualError::NotChronologicalOrder,
            );
            barrier.wait();
            break;
        }
        last_time1 = Some(time1);
        last_time2 = Some(time2);

        if time1 != time2 {
            error!(
                "Event sources differ starting at event #{}: time {time1} in {} and {time2} in {}",
                event_count + 1,
                source1.display(),
                source2.display()
            );
            stop_with_error(
                &comparison_result,
                &should_stop,
                EventsFileNotEqualError::DifferentEventTimes,
            );
            barrier.wait();
            break;
        }

        if events1.len() != events2.len() {
            stop_with_error(
                &comparison_result,
                &should_stop,
                EventsFileNotEqualError::DifferentNumberOfEvents,
            );
            barrier.wait();
            break;
        }
        if let Err(id) = compare_batch_of_events(&events1, &events2) {
            error!(
                "Event at time {time1} from {} is missing in {}: {:?}",
                source1.display(),
                source2.display(),
                events1[id]
            );
            stop_with_error(
                &comparison_result,
                &should_stop,
                EventsFileNotEqualError::MissingEvent {
                    event: format!("{:?}", events1[id]),
                },
            );
            barrier.wait();
            break;
        }

        record_processed_events(&mut event_count, &mut next_status_event, events1.len());
        barrier.wait();
    }
}

type EventBatch = (SimTime, Vec<GenericEvent>);

trait EventBatchReader {
    fn next_batch(&mut self) -> Option<EventBatch>;
}

struct XmlBatchReader {
    reader: XmlEventsReader,
    pending: Option<(SimTime, Box<dyn EventTrait>)>,
}

impl XmlBatchReader {
    fn new(path: &Path) -> Self {
        Self {
            reader: XmlEventsReader::new(path),
            pending: None,
        }
    }
}

impl EventBatchReader for XmlBatchReader {
    fn next_batch(&mut self) -> Option<EventBatch> {
        let (time, first_event) = self.pending.take().or_else(|| self.reader.read_next())?;
        let mut events = vec![event_to_proto(first_event.as_ref())];

        while let Some((next_time, event)) = self.reader.read_next() {
            if next_time == time {
                events.push(event_to_proto(event.as_ref()));
            } else {
                self.pending = Some((next_time, event));
                break;
            }
        }

        Some((time, events))
    }
}

struct ProtoBatchReader {
    reader: ProtoEventsReader<File>,
}

impl ProtoBatchReader {
    fn new(path: &Path) -> Self {
        Self {
            reader: ProtoEventsReader::from_file(path),
        }
    }
}

impl EventBatchReader for ProtoBatchReader {
    fn next_batch(&mut self) -> Option<EventBatch> {
        for (time, proto_events) in self.reader.by_ref() {
            if proto_events.is_empty() {
                continue;
            }
            return Some((time, proto_events));
        }
        None
    }
}

struct MergedBatchReader {
    readers: Vec<PreloadedReader>,
}

struct PreloadedReader {
    reader: Box<dyn EventBatchReader>,
    next: Option<EventBatch>,
}

impl MergedBatchReader {
    fn from_paths(paths: &[PathBuf]) -> Self {
        let readers = paths
            .iter()
            .map(|path| {
                let mut reader = event_file_reader(path);
                let next = reader.next_batch();
                PreloadedReader { reader, next }
            })
            .collect();
        Self { readers }
    }
}

impl EventBatchReader for MergedBatchReader {
    fn next_batch(&mut self) -> Option<EventBatch> {
        let time = self
            .readers
            .iter()
            .filter_map(|reader| reader.next.as_ref().map(|(time, _)| *time))
            .min()?;
        let mut events = Vec::new();

        loop {
            let mut consumed = false;
            for reader in &mut self.readers {
                if reader.next.as_ref().map(|(next_time, _)| *next_time) == Some(time) {
                    let (_, mut next_events) = reader.next.take().unwrap();
                    events.append(&mut next_events);
                    reader.next = reader.reader.next_batch();
                    consumed = true;
                }
            }
            if !consumed {
                break;
            }
        }

        Some((time, events))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EventFileFamily {
    Xml,
    Proto,
}

fn event_file_family(path: &Path) -> EventFileFamily {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("xml") | Some("gz") | Some("zst") => EventFileFamily::Xml,
        Some("binpb") | Some("pbf") => EventFileFamily::Proto,
        _ => panic!("Unsupported event file format: {}", path.display()),
    }
}

fn event_file_reader(path: &Path) -> Box<dyn EventBatchReader> {
    match event_file_family(path) {
        EventFileFamily::Xml => Box::new(XmlBatchReader::new(path)),
        EventFileFamily::Proto => Box::new(ProtoBatchReader::new(path)),
    }
}

enum EventSource {
    File(PathBuf),
    Folder(Vec<PathBuf>),
}

impl EventSource {
    fn into_reader(self) -> Box<dyn EventBatchReader> {
        match self {
            EventSource::File(path) => event_file_reader(&path),
            EventSource::Folder(paths) => Box::new(MergedBatchReader::from_paths(&paths)),
        }
    }
}

#[derive(Default)]
struct PublishedEventBatch {
    batch: Option<EventBatch>,
    finished: bool,
}

pub(super) fn compare_event_files(
    file1: &Path,
    file2: &Path,
) -> Result<(), EventsFileNotEqualError> {
    validate_event_file(file1);
    validate_event_file(file2);
    compare_sources(
        EventSource::File(file1.to_path_buf()),
        EventSource::File(file2.to_path_buf()),
        file1,
        file2,
    )
}

pub(super) fn compare_event_folder(
    folder1: &Path,
    folder2: &Path,
) -> Result<(), EventsFileNotEqualError> {
    let files1 = event_files_in_folder(folder1);
    let files2 = event_files_in_folder(folder2);
    compare_sources(
        EventSource::Folder(files1),
        EventSource::Folder(files2),
        folder1,
        folder2,
    )
}

fn validate_event_file(path: &Path) {
    assert!(
        path.is_file(),
        "Event file does not exist or is not a file: {}",
        path.display()
    );
    event_file_family(path);
}

fn event_files_in_folder(folder: &Path) -> Vec<PathBuf> {
    assert!(
        folder.is_dir(),
        "Event folder does not exist or is not a directory: {}",
        folder.display()
    );

    let mut files_by_rank = HashMap::new();
    let mut family = None;
    for entry in fs::read_dir(folder)
        .unwrap_or_else(|error| panic!("Failed to read event folder {}: {error}", folder.display()))
    {
        let path = entry.unwrap().path();
        if !path.is_file() {
            continue;
        }
        let Some(rank) = event_file_rank(&path) else {
            continue;
        };
        let current_family = event_file_family(&path);
        if let Some(expected_family) = family {
            assert_eq!(
                expected_family,
                current_family,
                "Mixed XML and protobuf event files in folder {}",
                folder.display()
            );
        } else {
            family = Some(current_family);
        }
        assert!(
            files_by_rank.insert(rank, path.clone()).is_none(),
            "Multiple event files for rank {rank} in folder {}",
            folder.display()
        );
    }

    assert!(
        !files_by_rank.is_empty(),
        "No files matching events.<rank>.<format> found in folder {}",
        folder.display()
    );

    let mut ranked_files: Vec<_> = files_by_rank.into_iter().collect();
    ranked_files.sort_by_key(|(rank, _)| *rank);
    for (expected_rank, (rank, _)) in ranked_files.iter().enumerate() {
        assert_eq!(
            expected_rank,
            *rank,
            "Event ranks in folder {} must be contiguous and start at 0",
            folder.display()
        );
    }
    ranked_files.into_iter().map(|(_, path)| path).collect()
}

fn event_file_rank(path: &Path) -> Option<usize> {
    let name = path.file_name()?.to_str()?;
    let rest = name.strip_prefix("events.")?;
    let rank = [".xml.gz", ".xml.zst", ".xml", ".binpb", ".pbf"]
        .iter()
        .find_map(|suffix| rest.strip_suffix(suffix))?;
    rank.parse().ok()
}

fn compare_sources(
    event_source1: EventSource,
    event_source2: EventSource,
    source_label1: &Path,
    source_label2: &Path,
) -> Result<(), EventsFileNotEqualError> {
    let batch1 = Arc::new(Mutex::new(PublishedEventBatch::default()));
    let batch2 = Arc::new(Mutex::new(PublishedEventBatch::default()));
    let comparison_result = Arc::new(Mutex::new(Ok(())));
    let should_stop = Arc::new(AtomicBool::new(false));

    // Barrier for 3 threads: 2 readers + 1 comparator
    let barrier = Arc::new(Barrier::new(3));

    let reader1 = spawn_event_reader(
        event_source1,
        Arc::clone(&batch1),
        Arc::clone(&should_stop),
        Arc::clone(&barrier),
    );
    let reader2 = spawn_event_reader(
        event_source2,
        Arc::clone(&batch2),
        Arc::clone(&should_stop),
        Arc::clone(&barrier),
    );

    let comparator_result = Arc::clone(&comparison_result);
    let source1 = source_label1.to_path_buf();
    let source2 = source_label2.to_path_buf();
    let comparator = thread::spawn(move || {
        let _guard = init_std_out_logging_thread_local();
        compare_published_batches(
            batch1,
            batch2,
            barrier,
            should_stop,
            comparator_result,
            source1,
            source2,
        );
    });

    reader1.join().unwrap();
    reader2.join().unwrap();
    comparator.join().unwrap();

    let result = comparison_result.lock().unwrap().clone();
    result
}

fn publish_batch(
    published_batch: &Arc<Mutex<PublishedEventBatch>>,
    batch: Option<EventBatch>,
    finished: bool,
) {
    let mut published_batch = published_batch.lock().unwrap();
    published_batch.batch = batch;
    published_batch.finished = finished;
}
fn take_published_batch(
    published_batch: &Arc<Mutex<PublishedEventBatch>>,
) -> (Option<EventBatch>, bool) {
    let mut published_batch = published_batch.lock().unwrap();
    let batch = published_batch.batch.take();
    (batch, published_batch.finished)
}

fn stop_with_error(
    comparison_result: &Arc<Mutex<Result<(), EventsFileNotEqualError>>>,
    should_stop: &Arc<AtomicBool>,
    error: EventsFileNotEqualError,
) {
    *comparison_result.lock().unwrap() = Err(error);
    should_stop.store(true, Ordering::Relaxed);
}

fn record_processed_events(
    event_count: &mut u64,
    next_status_event: &mut Option<u64>,
    processed_events: usize,
) {
    *event_count += processed_events as u64;
    while let Some(status_event) = *next_status_event {
        if *event_count < status_event {
            break;
        }
        info!("Processed event # {status_event}");
        *next_status_event = status_event.checked_mul(4);
    }
}

fn compare_batch_of_events(
    events1: &[GenericEvent],
    events2: &[GenericEvent],
) -> Result<(), usize> {
    let mut matched = HashSet::new();
    for (id1, event1) in events1.iter().enumerate() {
        let match_id = events2
            .iter()
            .enumerate()
            .find(|(id2, event2)| !matched.contains(id2) && event1 == *event2)
            .map(|(id2, _)| id2);
        if let Some(id2) = match_id {
            matched.insert(id2);
        } else {
            return Err(id1);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::simulation::events::EventsManager;
    use crate::simulation::events::utils::{
        EventsFileNotEqualError, compare_event_files, compare_event_folder, read_events,
    };
    use crate::simulation::io::proto::proto_events::ProtoEventsWriter;
    use macros::deterministic_id_test;
    use std::fs;
    use std::path::Path;

    fn write_xml(path: &Path, events: &str) {
        fs::write(
            path,
            format!("<?xml version=\"1.0\"?><events>{events}</events>"),
        )
        .unwrap();
    }

    #[deterministic_id_test]
    fn identical_xml_event_files_compare_equal() {
        compare_event_files(
            "./tests/resources/events/expected_events.xml",
            "./tests/resources/events/expected_events.xml",
        )
        .unwrap();
    }

    #[deterministic_id_test]
    fn same_time_event_order_is_ignored() {
        compare_event_files(
            "./tests/resources/events/expected_events.xml",
            "./tests/resources/events/expected_events_changed_order_legally.xml",
        )
        .unwrap();
    }

    #[deterministic_id_test]
    fn different_times_are_reported() {
        let result = compare_event_files(
            "./tests/resources/events/expected_events.xml",
            "./tests/resources/events/expected_events_changed_order_illegally.xml",
        );
        assert!(matches!(
            result,
            Err(EventsFileNotEqualError::DifferentEventTimes)
        ));
    }

    #[deterministic_id_test]
    fn non_chronological_sources_are_reported() {
        let file = "./tests/resources/events/expected_events_changed_order_illegally.xml";
        let result = compare_event_files(file, file);
        assert!(matches!(
            result,
            Err(EventsFileNotEqualError::NotChronologicalOrder)
        ));
    }

    #[deterministic_id_test]
    fn different_event_data_is_reported() {
        let result = compare_event_files(
            "./tests/resources/events/expected_events.xml",
            "./tests/resources/events/expected_events_modified_data.xml",
        );
        assert!(matches!(
            result,
            Err(EventsFileNotEqualError::MissingEvent { .. })
        ));
    }

    #[deterministic_id_test]
    fn different_event_counts_are_reported() {
        let result = compare_event_files(
            "./tests/resources/events/expected_events.xml",
            "./tests/resources/events/expected_events_removed_events.xml",
        );
        assert!(matches!(
            result,
            Err(EventsFileNotEqualError::DifferentNumberOfEvents)
        ));
    }

    #[deterministic_id_test]
    fn xml_and_proto_files_compare_equal() {
        let temp = tempfile::tempdir().unwrap();
        let proto_path = temp.path().join("events.binpb");
        let mut manager = EventsManager::new();
        ProtoEventsWriter::register_fn(proto_path.clone())(&mut manager);
        read_events(
            &mut manager,
            "./tests/resources/events/expected_events.0.xml",
        )
        .unwrap();

        compare_event_files(
            "./tests/resources/events/expected_events.0.xml",
            &proto_path,
        )
        .unwrap();
        compare_event_files(
            &proto_path,
            "./tests/resources/events/expected_events.0.xml",
        )
        .unwrap();
    }

    #[deterministic_id_test]
    fn folders_compare_independently_of_partition_count() {
        let temp = tempfile::tempdir().unwrap();
        let single = temp.path().join("single");
        let partitioned = temp.path().join("partitioned");
        fs::create_dir_all(&single).unwrap();
        fs::create_dir_all(&partitioned).unwrap();

        let event1 = "<event time=\"1\" type=\"left link\" link=\"link1\" vehicle=\"vehicle1\"/>";
        let event2 =
            "<event time=\"2\" type=\"entered link\" link=\"link2\" vehicle=\"vehicle1\"/>";
        write_xml(&single.join("events.0.xml"), &format!("{event1}{event2}"));
        write_xml(&partitioned.join("events.0.xml"), event1);
        write_xml(&partitioned.join("events.1.xml"), event2);

        compare_event_folder(single, partitioned).unwrap();
    }

    #[deterministic_id_test]
    #[should_panic(expected = "contiguous")]
    fn folder_ranks_must_be_contiguous() {
        let temp = tempfile::tempdir().unwrap();
        write_xml(&temp.path().join("events.0.xml"), "");
        write_xml(&temp.path().join("events.2.xml"), "");
        let _ = compare_event_folder(temp.path(), temp.path());
    }

    #[deterministic_id_test]
    #[should_panic(expected = "Mixed XML and protobuf")]
    fn folder_must_not_mix_xml_and_proto() {
        let temp = tempfile::tempdir().unwrap();
        write_xml(&temp.path().join("events.0.xml"), "");
        fs::write(temp.path().join("events.1.binpb"), []).unwrap();
        let _ = compare_event_folder(temp.path(), temp.path());
    }

    #[deterministic_id_test]
    #[should_panic(expected = "does not exist or is not a directory")]
    fn event_folder_must_exist() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing");
        let _ = compare_event_folder(&missing, &missing);
    }

    #[deterministic_id_test]
    #[should_panic(expected = "No files matching events.<rank>.<format>")]
    fn event_folder_must_contain_event_files() {
        let temp = tempfile::tempdir().unwrap();
        let _ = compare_event_folder(temp.path(), temp.path());
    }

    #[deterministic_id_test]
    #[should_panic(expected = "Multiple event files for rank 0")]
    fn folder_must_not_contain_duplicate_ranks() {
        let temp = tempfile::tempdir().unwrap();
        write_xml(&temp.path().join("events.0.xml"), "");
        fs::write(temp.path().join("events.0.xml.gz"), []).unwrap();
        let _ = compare_event_folder(temp.path(), temp.path());
    }
}
