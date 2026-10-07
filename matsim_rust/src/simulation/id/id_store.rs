use bytes::{Buf, BufMut};
use dashmap::DashMap;
use lz4::BlockMode;
use prost::Message;
use prost::encoding::{DecodeContext, WireType};
use std::fmt::{Debug, Formatter};
use std::fs;
use std::fs::File;
use std::io::{BufReader, BufWriter, Cursor, Read, Seek, Write};
use std::path::Path;
use std::sync::Arc;
use tracing::info;

use crate::generated::MessageIter;
use crate::generated::ids::IdsWithType;
use crate::generated::ids::ids_with_type::Data;
use crate::simulation::id::Id;
use crate::simulation::id::serializable_type::StableTypeId;

#[derive(Clone, Copy)]
#[allow(dead_code)] // allow dead code, because we never construct None. I still want to have it as option here.
enum IdCompression {
    LZ4,
    None,
}

fn serialize_to_file(store: &IdStore, file_path: &Path, compression: IdCompression) {
    info!("Starting writing IdStore to file {file_path:?}");
    // Create the file and all necessary directories
    let prefix = file_path.parent().unwrap();
    fs::create_dir_all(prefix).unwrap();
    let file = File::create(file_path).unwrap();

    let mut file_writer = BufWriter::new(file);
    serialize(store, &mut file_writer, compression);
    info!("Finished writing IdStore to file {file_path:?}");
}

fn serialize<W: Write>(store: &IdStore, writer: &mut W, compression: IdCompression) {
    for entry in &store.ids {
        let type_id = entry.key();
        let ids = entry.value();
        let data = serialize_ids(ids, compression);
        let ids = IdsWithType {
            type_id: *type_id,
            data: Some(data),
        };
        let encoded_typed_ids = ids.encode_length_delimited_to_vec();
        writer
            .write_all(&encoded_typed_ids)
            .expect("Failed to write encoded type ids to writer.");
    }
    writer
        .flush()
        .expect("Failed to flush writer after serializing id store");
}

fn deserialize_from_file(store: &IdStore, file_path: &Path) {
    info!("Starting to load IdStore from file {file_path:?}");
    let file = File::open(file_path).unwrap();
    let mut file_reader = BufReader::new(file);
    deserialize(store, &mut file_reader);
}

/// This method takes a BufReader instance as we are relying on 'seek_relative' which is not part of
/// the Read trait. I think it is ok, to let callees wrap their bytes into a BufReader.
fn deserialize<R: Read + Seek>(store: &IdStore, reader: R) {
    info!("Starting to de-serialize Id store.");
    let delim_reader: MessageIter<IdsWithType, R> = MessageIter::new(reader);
    for message in delim_reader {
        let ids = deserialize_ids(&message);
        store.replace_ids(&ids, message.type_id);
    }

    info!("Finished de-serializing id store.");
}

fn serialize_ids(ids: &Vec<Arc<UntypedId>>, mode: IdCompression) -> Data {
    match mode {
        IdCompression::LZ4 => serialize_ids_compressed(ids),
        IdCompression::None => serialize_ids_uncompressed(ids),
    }
}

fn serialize_ids_uncompressed(ids: &Vec<Arc<UntypedId>>) -> Data {
    let mut writer = BufWriter::new(Vec::new());
    encode_ids(ids, &mut writer);

    let bytes = writer
        .into_inner()
        .expect("Failed to transform writer into_inner as Vec<u8>");
    Data::Raw(bytes)
}

fn serialize_ids_compressed(ids: &Vec<Arc<UntypedId>>) -> Data {
    let mut writer = Vec::new();
    {
        let mut encoder = lz4::EncoderBuilder::new()
            .block_mode(BlockMode::Independent)
            .build(&mut writer)
            .expect("Failed to create LZ4 encoder");

        encode_ids(ids, &mut encoder);

        let (_output, result) = encoder.finish();
        result.expect("Failed to finish LZ4 encoding");
    }
    Data::Lz4Data(writer)
}

fn encode_ids<W: Write>(ids: &Vec<Arc<UntypedId>>, writer: &mut W) {
    let mut id_buffer = Vec::new();

    for id in ids {
        prost::encoding::encode_varint(id.external.len() as u64, &mut id_buffer);
        id_buffer.put_slice(id.external.as_bytes());
        writer
            .write_all(&id_buffer)
            .expect("Failed to write encoded String.");
        id_buffer.clear();
    }
    writer.flush().expect("Failed to flush writer.");
}

fn deserialize_ids(ids: &IdsWithType) -> Vec<String> {
    if let Some(bytes) = &ids.data {
        match bytes {
            Data::Raw(raw_bytes) => deserialize_ids_uncompressed(raw_bytes),
            Data::Lz4Data(lz4_bytes) => deserialize_ids_compressed(lz4_bytes),
        }
    } else {
        Vec::new()
    }
}

fn deserialize_ids_compressed(bytes: &[u8]) -> Vec<String> {
    let compressed_reader = Cursor::new(bytes);
    let mut decompressor = lz4_flex::frame::FrameDecoder::new(compressed_reader);

    let mut uncompressed_bytes = Vec::new();
    decompressor
        .read_to_end(&mut uncompressed_bytes)
        .expect("Failed to de-compress bytes");

    let mut uncompressed_reader = Cursor::new(uncompressed_bytes);
    decode_ids(&mut uncompressed_reader)
}

fn deserialize_ids_uncompressed(bytes: &[u8]) -> Vec<String> {
    let mut cursor = Cursor::new(bytes);
    decode_ids(&mut cursor)
}

fn decode_ids<B: Buf>(buffer: &mut B) -> Vec<String> {
    let mut result = Vec::new();

    while buffer.has_remaining() {
        let mut external_id = String::new();
        prost::encoding::string::merge(
            WireType::LengthDelimited,
            &mut external_id,
            buffer,
            DecodeContext::default(),
        )
        .expect("Error decoding String");

        result.push(external_id);
    }
    result
}

pub struct UntypedId {
    pub(crate) internal: u64,
    // Shared immutable id text. This is cloned into reverse-lookup maps without copying bytes.
    // Not using &str here to ensure memory safety, i.e., make sure that the reference is always valid.
    // Not using String here because essentially a copy of String would be necessary, which is not memory efficient.
    pub(crate) external: Arc<str>,
}

impl Debug for UntypedId {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.external)
    }
}

impl UntypedId {
    pub(crate) fn new(internal: u64, external: impl Into<Arc<str>>) -> Self {
        Self {
            internal,
            external: external.into(),
        }
    }
}

#[derive(Debug)]
pub struct IdStore {
    // Dense per-type storage: internal id == index in this vector.
    ids: DashMap<u64, Vec<Arc<UntypedId>>>,
    // Per-type reverse lookup by external id text.
    mapping: DashMap<u64, DashMap<Arc<str>, u64>>,
}

/// Cache for ids. All methods are public, so that they can be used from mod.rs. The module doesn't
/// export this module, so that everything is kept package private
impl IdStore {
    pub fn new() -> Self {
        Self {
            ids: DashMap::default(),
            mapping: DashMap::default(),
        }
    }

    fn create_id_with_type_id(&self, id: &str, type_id: u64) -> Arc<UntypedId> {
        let type_mapping = self.mapping.entry(type_id).or_default();

        // First check if the ID already exists
        if let Some(internal) = type_mapping.get(id) {
            return self
                .ids
                .get(&type_id)
                .unwrap()
                .get(*internal as usize)
                .unwrap()
                .clone();
        }

        // If not, create a new one
        let mut type_ids = self.ids.entry(type_id).or_default();
        let next_internal = type_ids.len() as u64;
        let next_id = Arc::new(UntypedId::new(next_internal, id));
        type_ids.push(next_id.clone());
        type_mapping.insert(next_id.external.clone(), next_id.internal);
        next_id
    }

    fn replace_ids(&self, ids: &Vec<String>, type_id: u64) {
        if let Some(type_mapping) = self.mapping.get_mut(&type_id) {
            type_mapping.clear();
        }
        if let Some(mut type_ids) = self.ids.get_mut(&type_id) {
            type_ids.clear();
        }

        for external_id in ids {
            self.create_id_with_type_id(external_id, type_id);
        }
    }

    pub(crate) fn create_id<T: StableTypeId>(&self, id: &str) -> Id<T> {
        let type_id = T::stable_type_id();
        Id::new(self.create_id_with_type_id(id, type_id))
    }

    pub(crate) fn get<T: StableTypeId>(&self, internal: u64) -> Id<T> {
        let type_id = T::stable_type_id();
        let type_ids = self.ids.get(&type_id).unwrap_or_else(|| {
            panic!("No ids for type {type_id:?}. Use Id::create::<T>(...) to create ids")
        });

        let untyped_id = type_ids
            .get(internal as usize)
            .unwrap_or_else(|| panic!("No id found for internal {internal}"))
            .clone();
        Id::new(untyped_id)
    }

    pub(crate) fn try_get_from_ext<T: StableTypeId>(&self, external: &str) -> Option<Id<T>> {
        let type_id = T::stable_type_id();
        let type_mapping = self.mapping.get(&type_id)?;

        let index = type_mapping.get(external)?;
        let id = self.get(*index);
        Some(id)
    }

    pub(crate) fn get_from_ext<T: StableTypeId>(&self, external: &str) -> Id<T> {
        let type_id = T::stable_type_id();
        let type_mapping = self.mapping.get(&type_id).unwrap_or_else(|| {
            panic!("No ids for type {type_id:?}. Use Id::create::<T>(...) to create ids. Requested external id: {external}");
        });

        let index = type_mapping.get(external).unwrap_or_else(|| {
            panic!("Could not find id for external id: {external}");
        });

        self.get(*index)
    }

    pub(crate) fn to_file(&self, file_path: &Path) {
        serialize_to_file(self, file_path, IdCompression::LZ4);
    }

    pub(crate) fn load_from_file(&self, file_path: &Path) {
        deserialize_from_file(self, file_path);
    }

    #[cfg(any(test, feature = "test_util"))]
    pub(crate) fn reset(&self) {
        self.ids.clear();
        self.mapping.clear();
    }
}

#[cfg(test)]
mod tests {
    use crate::simulation::config::PartitionMethod;
    use crate::simulation::id::Id;
    use crate::simulation::id::id_store::{
        IdCompression, IdStore, deserialize, deserialize_from_file, serialize, serialize_to_file,
    };
    use crate::simulation::logging::init_std_out_logging_thread_local;
    use crate::simulation::scenario::network::{Link, Network, Node};
    use crate::simulation::scenario::population::InternalPerson;
    use crate::simulation::scenario::population::Population;
    use crate::simulation::scenario::vehicles::Garage;
    use crate::simulation::scenario::vehicles::{InternalVehicle, InternalVehicleType};
    use crate::test_utils::create_folders;
    use macros::deterministic_id_test;
    use std::io::{BufReader, BufWriter, Cursor};
    use std::ops::Sub;
    use std::path::PathBuf;
    use std::thread;
    use std::time::Instant;

    #[test]
    fn write_read_ids_store() {
        let folder = create_folders(PathBuf::from(
            "./test_output/simulation/id/id_store/write_read_ids_store/",
        ));
        let file = folder.join("ids.pbf");
        let store = IdStore::new();
        store.create_id::<()>("test-1");
        store.create_id::<()>("test-2");
        store.create_id::<String>("string-id");

        serialize_to_file(&store, &file, IdCompression::LZ4);
        let mut result = IdStore::new();
        deserialize_from_file(&mut result, &file);

        println!("{result:?}");

        assert_eq!(
            store.get_from_ext::<()>("test-1"),
            result.get_from_ext::<()>("test-1")
        );
        assert_eq!(
            store.get_from_ext::<String>("string-id"),
            result.get_from_ext::<String>("string-id")
        );
    }

    #[test]
    fn write_read_ids_store_uncompressed() {
        let folder = create_folders(PathBuf::from(
            "./test_output/simulation/id/id_store/write_read_ids_store_uncompressed/",
        ));
        let file = folder.join("ids.pbf");
        let store = IdStore::new();
        store.create_id::<()>("test-1");
        store.create_id::<()>("test-2");
        store.create_id::<String>("string-id");

        serialize_to_file(&store, &file, IdCompression::None);
        let mut result = IdStore::new();
        deserialize_from_file(&mut result, &file);

        println!("{result:?}");

        assert_eq!(
            store.get_from_ext::<()>("test-1"),
            result.get_from_ext::<()>("test-1")
        );
        assert_eq!(
            store.get_from_ext::<String>("string-id"),
            result.get_from_ext::<String>("string-id")
        );
    }

    #[test]
    fn test_serialize_ids() {
        let store = IdStore::new();
        store.create_id::<()>("test-1");
        store.create_id::<()>("test-2");
        store.create_id::<String>("string-id");

        let mut serialized_bytes = Vec::new();
        let mut writer = BufWriter::new(serialized_bytes);
        serialize(&store, &mut writer, IdCompression::LZ4);

        serialized_bytes = writer
            .into_inner()
            .expect("Failed to transform into inner.");

        println!("{serialized_bytes:?}");

        let mut vec_reader = BufReader::new(Cursor::new(serialized_bytes));
        let mut result = IdStore::new();
        deserialize(&mut result, &mut vec_reader);

        println!("{result:?}");

        assert_eq!(
            store.get_from_ext::<()>("test-1"),
            result.get_from_ext::<()>("test-1")
        );
        assert_eq!(
            store.get_from_ext::<String>("string-id"),
            result.get_from_ext::<String>("string-id")
        );
    }

    #[test]
    #[ignore]
    fn compare_compression() {
        let _g = init_std_out_logging_thread_local();
        let folder = create_folders(PathBuf::from(
            "./test_output/simulation/id/id_store/compare_compression/",
        ));
        let store = IdStore::new();

        let net = Network::from_file_path(
            &PathBuf::from("/Users/janek/Documents/rust_qsim/input/rvr.network.xml.gz"),
            1,
            &PartitionMethod::None,
        );
        for link in net.links() {
            store.create_id::<Link>(link.id.external());
        }
        for node in net.nodes() {
            store.create_id::<Node>(node.id.external());
        }

        let mut garage = Garage::from_file(&PathBuf::from(
            "/Users/janek/Documents/rust_qsim/input/rvr.vehicles.xml",
        ));
        let pop = Population::from_file(
            &PathBuf::from("/Users/janek/Documents/rust_qsim/input/rvr-10pct.plans.xml.gz"),
            &mut garage,
        );

        for p_id in pop.persons.keys() {
            store.create_id::<InternalPerson>(p_id.external());
        }

        for v_id in garage.vehicles.keys() {
            store.create_id::<InternalVehicle>(v_id.external());
        }

        for t_id in garage.vehicle_types.keys() {
            store.create_id::<InternalVehicleType>(t_id.external());
        }

        println!("Starting to write id store raw");
        let start = Instant::now();
        serialize_to_file(&store, &folder.join("ids.raw.pbf"), IdCompression::None);
        let end = Instant::now();
        let duration = end.sub(start).as_millis();
        println!("writing uncompressed took: {duration}ms");

        println!("Starting to write id store compressed");
        let start = Instant::now();
        serialize_to_file(&store, &folder.join("ids.lz4.pbf"), IdCompression::LZ4);
        let end = Instant::now();
        let duration = end.sub(start).as_millis();
        println!("writing compressed took: {duration}ms");

        println!("Starting to read id store uncompressed");
        let start = Instant::now();
        let mut result_uncompressed = IdStore::new();
        deserialize_from_file(&mut result_uncompressed, &folder.join("ids.raw.pbf"));
        let end = Instant::now();
        let duration = end.sub(start).as_millis();
        println!("reading uncompressed took: {duration}ms");

        println!("Starting to read id store compressed");
        let start = Instant::now();
        let mut result_compressed = IdStore::new();
        deserialize_from_file(&mut result_compressed, &folder.join("ids.lz4.pbf"));
        let end = Instant::now();
        let duration = end.sub(start).as_millis();
        println!("reading compressed took: {duration}ms");
    }

    #[deterministic_id_test]
    fn performance_test_bulk_operations() {
        use std::time::Instant;

        const NUM_IDS: usize = 1_000_000;
        const NUM_THREADS: usize = 10;
        let external_ids: Vec<String> = (0..NUM_IDS).map(|i| format!("test-id-{}", i)).collect();

        // Bulk creation phase
        let start = Instant::now();
        let mut internal_ids = Vec::with_capacity(NUM_IDS);
        for ext_id in &external_ids {
            let id = Id::<()>::create(ext_id);
            internal_ids.push(id.internal());
        }
        let creation_time = start.elapsed();
        println!("\n=== Bulk Creation/Parallel Lookup Test ===");
        println!("Creating {NUM_IDS} IDs took: {:?}", creation_time);
        println!(
            "Average time per ID creation: {:?}",
            creation_time / NUM_IDS as u32
        );

        // Pre-split data into chunks for threads
        let chunk_size = NUM_IDS / NUM_THREADS;
        let mut ext_id_chunks = Vec::with_capacity(NUM_THREADS);
        let mut int_id_chunks = Vec::with_capacity(NUM_THREADS);

        for thread_idx in 0..NUM_THREADS {
            let start_idx = thread_idx * chunk_size;
            let end_idx = if thread_idx == NUM_THREADS - 1 {
                NUM_IDS
            } else {
                (thread_idx + 1) * chunk_size
            };

            // Create owned chunks for each thread
            ext_id_chunks.push(external_ids[start_idx..end_idx].to_vec());
            int_id_chunks.push(internal_ids[start_idx..end_idx].to_vec());
        }

        // Parallel bulk lookup phase
        let start = Instant::now();
        let mut handles = Vec::with_capacity(NUM_THREADS);

        for _ in 0..NUM_THREADS {
            let ext_chunk = ext_id_chunks.remove(0);
            let int_chunk = int_id_chunks.remove(0);

            let handle = thread::spawn(move || {
                for i in 0..ext_chunk.len() {
                    if i % 2 == 0 {
                        let _ = Id::<()>::get_from_ext(&ext_chunk[i]);
                    } else {
                        let _ = Id::<()>::get(int_chunk[i]);
                    }
                }
            });
            handles.push(handle);
        }

        // Wait for all threads to complete
        for handle in handles {
            handle.join().unwrap();
        }

        let lookup_time = start.elapsed();
        println!(
            "Looking up {NUM_IDS} IDs with {NUM_THREADS} threads took: {:?}",
            lookup_time
        );
        println!(
            "Average time per ID lookup: {:?}",
            lookup_time / NUM_IDS as u32
        );
    }

    #[deterministic_id_test]
    fn performance_test_interleaved_operations() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::Instant;

        const NUM_IDS: usize = 1_000_000;
        const INITIAL_BATCH_SIZE: usize = (NUM_IDS as f64 * 0.95) as usize;
        const REMAINING_IDS: usize = NUM_IDS - INITIAL_BATCH_SIZE;
        const NUM_THREADS: usize = 10;

        let all_external_ids: Vec<String> =
            (0..NUM_IDS).map(|i| format!("test-id-{}", i)).collect();

        // Initial batch creation (95%)
        println!("\n=== Interleaved Creation/Parallel Lookup Test ===");
        let start = Instant::now();
        let mut internal_ids = Vec::with_capacity(INITIAL_BATCH_SIZE);
        for ext_id in &all_external_ids[0..INITIAL_BATCH_SIZE] {
            let id = Id::<()>::create(ext_id);
            internal_ids.push(id.internal());
        }
        let initial_creation_time = start.elapsed();
        println!(
            "Creating initial {INITIAL_BATCH_SIZE} IDs took: {:?}",
            initial_creation_time
        );

        // Pre-split data into chunks for threads
        let chunk_size = INITIAL_BATCH_SIZE / NUM_THREADS;
        let mut ext_id_chunks = Vec::with_capacity(NUM_THREADS);
        let mut int_id_chunks = Vec::with_capacity(NUM_THREADS);
        let mut remaining_chunks = Vec::with_capacity(NUM_THREADS);

        for thread_idx in 0..NUM_THREADS {
            let start_idx = thread_idx * chunk_size;
            let end_idx = if thread_idx == NUM_THREADS - 1 {
                INITIAL_BATCH_SIZE
            } else {
                (thread_idx + 1) * chunk_size
            };

            // Create owned chunks for each thread
            ext_id_chunks.push(all_external_ids[start_idx..end_idx].to_vec());
            int_id_chunks.push(internal_ids[start_idx..end_idx].to_vec());

            // Pre-split remaining IDs for creation
            let remaining_start = INITIAL_BATCH_SIZE + thread_idx * (REMAINING_IDS / NUM_THREADS);
            let remaining_end = if thread_idx == NUM_THREADS - 1 {
                NUM_IDS
            } else {
                INITIAL_BATCH_SIZE + (thread_idx + 1) * (REMAINING_IDS / NUM_THREADS)
            };
            remaining_chunks.push(all_external_ids[remaining_start..remaining_end].to_vec());
        }

        let total_lookups = std::sync::Arc::new(AtomicUsize::new(0));
        let total_creations = std::sync::Arc::new(AtomicUsize::new(0));

        // Parallel interleaved operations
        let start = Instant::now();
        let mut handles = Vec::with_capacity(NUM_THREADS);

        for _ in 0..NUM_THREADS {
            let ext_chunk = ext_id_chunks.remove(0);
            let int_chunk = int_id_chunks.remove(0);
            let remaining_chunk = remaining_chunks.remove(0);
            let total_lookups = total_lookups.clone();
            let total_creations = total_creations.clone();

            let handle = thread::spawn(move || {
                let mut local_lookup_count = 0;
                let mut local_creation_idx = 0;

                // Process chunk of existing IDs
                for i in 0..ext_chunk.len() {
                    // Alternate between get_from_ext and get for existing IDs
                    if i % 2 == 0 {
                        let _ = Id::<()>::get_from_ext(&ext_chunk[i]);
                    } else {
                        let _ = Id::<()>::get(int_chunk[i]);
                    }
                    local_lookup_count += 1;

                    // Create new IDs from the remaining chunk
                    if local_lookup_count % 20 == 0 && local_creation_idx < remaining_chunk.len() {
                        Id::<()>::create(&remaining_chunk[local_creation_idx]);
                        local_creation_idx += 1;
                    }
                }
                total_lookups.fetch_add(local_lookup_count, Ordering::Relaxed);
                total_creations.fetch_add(local_creation_idx, Ordering::Relaxed);
            });
            handles.push(handle);
        }

        // Wait for all threads to complete
        for handle in handles {
            handle.join().unwrap();
        }

        let lookups_performed = total_lookups.load(Ordering::Relaxed);
        let creations_performed = total_creations.load(Ordering::Relaxed);

        let interleaved_time = start.elapsed();
        println!(
            "Interleaved operations with {NUM_THREADS} threads took: {:?}",
            interleaved_time
        );
        println!(
            "Performed {} lookups and {} creations",
            lookups_performed, creations_performed
        );
        println!(
            "Average time per operation: {:?}",
            interleaved_time / (lookups_performed + creations_performed) as u32
        );
    }
}
