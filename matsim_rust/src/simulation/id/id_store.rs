use arc_swap::ArcSwap;
use bytes::{Buf, BufMut};
use dashmap::DashMap;
use lz4::BlockMode;
use nohash_hasher::IntMap;
use prost::Message;
use prost::encoding::{DecodeContext, WireType};
use std::fmt::{Debug, Formatter};
use std::fs;
use std::fs::File;
use std::io::{BufReader, BufWriter, Cursor, Read, Seek, Write};
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};
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
    for (type_id, ids) in store.sorted_ids() {
        let data = serialize_ids(&ids, compression);
        let ids = IdsWithType {
            type_id,
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

/// Hasher for the lookup maps of the store. The seeds are fixed, so that the store does not depend
/// on a process-random state. Hash values never influence internal ids, see [`IdStore`].
type LookupHasher = ahash::RandomState;

fn lookup_hasher() -> LookupHasher {
    LookupHasher::with_seeds(
        0x243f_6a88_85a3_08d3,
        0x1319_8a2e_0370_7344,
        0xa409_3822_299f_31d0,
        0x082e_fa98_ec4e_6c89,
    )
}

/// All ids of one stable type id.
#[derive(Debug)]
struct TypeIds {
    // Dense storage: internal id == index in this vector. Its write lock also serializes the creation of ids of this type.
    // RwLock allows multiple readers but only one writer at a time.
    ids: RwLock<Vec<Arc<UntypedId>>>,
    // Reverse lookup by external id text. It points to the same shared ids as `ids`, so that a lookup by external id needs only one map access.
    mapping: DashMap<Arc<str>, Arc<UntypedId>, LookupHasher>,
}

impl TypeIds {
    fn new() -> Self {
        Self {
            ids: RwLock::new(Vec::new()),
            mapping: DashMap::with_hasher(lookup_hasher()),
        }
    }

    fn create(&self, external: &str) -> Arc<UntypedId> {
        let mut ids = self.ids.write().expect("Id store lock is poisoned.");
        // First check if the ID already exists. Another thread might have created it while this
        // thread was waiting for the lock.
        if let Some(existing) = self.mapping.get(external) {
            return existing.clone();
        }

        // If not, create a new one
        let next_id = Arc::new(UntypedId::new(ids.len() as u64, external));
        ids.push(next_id.clone());
        self.mapping
            .insert(next_id.external.clone(), next_id.clone());
        next_id
    }
}

/// Internal ids are assigned exclusively when an id is created: the internal id is the index at
/// which the id is pushed into the dense per-type vector. Serialization and
/// [`IdStore::replace_ids`] use these vectors as well. The hash maps are only used to look ids up and
/// are never iterated, so the hasher (and its implementation in a future version of `ahash`) can't
/// change the order of internal ids.
#[derive(Debug)]
pub struct IdStore {
    // Ids by stable type id. Reading the map is lock-free, so that parallel lookups don't contend
    // on a shared lock. The map is replaced as a whole when a new type is added, which happens
    // rarely.
    types: ArcSwap<IntMap<u64, Arc<TypeIds>>>,
    // Needed to serialize replacing the map of types. This is only used when a new type is added, which happens rarely.
    types_update: Mutex<()>,
}

/// Cache for ids. All methods are public, so that they can be used from mod.rs. The module doesn't
/// export this module, so that everything is kept package private
impl IdStore {
    pub fn new() -> Self {
        Self {
            types: ArcSwap::from_pointee(IntMap::default()),
            types_update: Mutex::new(()),
        }
    }

    fn type_ids_or_insert(&self, type_id: u64) -> Arc<TypeIds> {
        if let Some(type_ids) = self.types.load().get(&type_id) {
            return type_ids.clone();
        }

        // wait until the lock is released so that only one thread creates the new type ids and updates the map of types.
        let _guard = self
            .types_update
            .lock()
            .expect("Id store lock is poisoned.");
        let types = self.types.load_full();
        if let Some(type_ids) = types.get(&type_id) {
            return type_ids.clone();
        }
        let type_ids = Arc::new(TypeIds::new());
        let mut new_types = IntMap::clone(&types);
        new_types.insert(type_id, type_ids.clone());
        self.types.store(Arc::new(new_types));
        type_ids
    }

    fn lookup(&self, external: &str, type_id: u64) -> Option<Arc<UntypedId>> {
        let types = self.types.load();
        let id = types.get(&type_id)?.mapping.get(external)?.clone();
        Some(id)
    }

    fn create_id_with_type_id(&self, id: &str, type_id: u64) -> Arc<UntypedId> {
        // Most calls ask for ids which already exist. Looking them up doesn't take a write lock,
        // so that such calls don't block each other when they run in parallel.
        if let Some(existing) = self.lookup(id, type_id) {
            return existing;
        }
        self.type_ids_or_insert(type_id).create(id)
    }

    fn replace_ids(&self, ids: &Vec<String>, type_id: u64) {
        let type_ids = self.type_ids_or_insert(type_id);
        {
            let mut existing = type_ids.ids.write().expect("Id store lock is poisoned.");
            type_ids.mapping.clear();
            existing.clear();
        }

        for external_id in ids {
            type_ids.create(external_id);
        }
    }

    pub(crate) fn create_id<T: StableTypeId>(&self, id: &str) -> Id<T> {
        let type_id = T::stable_type_id();
        Id::new(self.create_id_with_type_id(id, type_id))
    }

    pub(crate) fn get<T: StableTypeId>(&self, internal: u64) -> Id<T> {
        let type_id = T::stable_type_id();
        let types = self.types.load();
        let type_ids = types.get(&type_id).unwrap_or_else(|| {
            panic!("No ids for type {type_id:?}. Use Id::create::<T>(...) to create ids")
        });

        let untyped_id = type_ids
            .ids
            .read()
            .expect("Id store lock is poisoned.")
            .get(internal as usize)
            .unwrap_or_else(|| panic!("No id found for internal {internal}"))
            .clone();
        Id::new(untyped_id)
    }

    pub(crate) fn try_get_from_ext<T: StableTypeId>(&self, external: &str) -> Option<Id<T>> {
        let type_id = T::stable_type_id();
        self.lookup(external, type_id).map(Id::new)
    }

    pub(crate) fn get_from_ext<T: StableTypeId>(&self, external: &str) -> Id<T> {
        let type_id = T::stable_type_id();
        // This call fixes the pointer. Subsequently, there will be no consistency problems within this method, even if another thread replaces the map of types.
        let types = self.types.load();
        let type_ids = types.get(&type_id).unwrap_or_else(|| {
            panic!("No ids for type {type_id:?}. Use Id::create::<T>(...) to create ids. Requested external id: {external}");
        });

        let id = type_ids.mapping.get(external).unwrap_or_else(|| {
            panic!("Could not find id for external id: {external}");
        });

        Id::new(id.clone())
    }

    pub(crate) fn count(&self) -> usize {
        let types = self.types.load();
        types
            .values()
            .map(|type_ids| {
                type_ids
                    .ids
                    .read()
                    .expect("Id store lock is poisoned.")
                    .len()
            })
            .sum()
    }

    /// Returns the ids of each type, ordered by type id and, within a type, by internal id.
    fn sorted_ids(&self) -> Vec<(u64, Vec<Arc<UntypedId>>)> {
        let types = self.types.load();
        let mut result: Vec<_> = types
            .iter()
            .map(|(type_id, type_ids)| {
                let ids = type_ids.ids.read().expect("Id store lock is poisoned.");
                (*type_id, ids.clone())
            })
            .collect();
        result.sort_by_key(|(type_id, _)| *type_id);
        result
    }

    pub(crate) fn to_file(&self, file_path: &Path) {
        serialize_to_file(self, file_path, IdCompression::LZ4);
    }

    pub(crate) fn load_from_file(&self, file_path: &Path) {
        deserialize_from_file(self, file_path);
    }

    #[cfg(any(test, feature = "test_util"))]
    pub(crate) fn reset(&self) {
        let _guard = self
            .types_update
            .lock()
            .expect("Id store lock is poisoned.");
        self.types.store(Arc::new(IntMap::default()));
    }

    /// Returns the external ids of each type, ordered by their internal id.
    #[cfg(any(test, feature = "test_util"))]
    pub(crate) fn snapshot(&self) -> std::collections::BTreeMap<u64, Vec<String>> {
        self.sorted_ids()
            .into_iter()
            .map(|(type_id, ids)| {
                let externals = ids.iter().map(|id| id.external.to_string());
                (type_id, externals.collect())
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use crate::simulation::config::PartitionMethod;
    use crate::simulation::id::Id;
    use crate::simulation::id::id_store::{
        IdCompression, IdStore, deserialize, deserialize_from_file, serialize, serialize_to_file,
    };
    use crate::simulation::id::serializable_type::StableTypeId;
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
    fn internal_ids_follow_creation_order() {
        let store = IdStore::new();
        let externals = ["z", "a", "m", "b", "y"];
        for external in externals {
            store.create_id::<String>(external);
        }
        // Creating existing ids again must neither change nor add internal ids.
        for external in externals.iter().rev() {
            store.create_id::<String>(external);
        }
        store.create_id::<Link>("a");

        for (internal, external) in externals.iter().enumerate() {
            let id = store.get_from_ext::<String>(external);
            assert_eq!(internal as u64, id.internal());
            assert_eq!(*external, store.get::<String>(internal as u64).external());
        }
        assert_eq!(0, store.get_from_ext::<Link>("a").internal());
        let snapshot = store.snapshot();
        assert_eq!(
            externals.to_vec(),
            snapshot[&String::stable_type_id()].as_slice()
        );

        let mut bytes = Vec::new();
        serialize(&store, &mut bytes, IdCompression::LZ4);
        let result = IdStore::new();
        deserialize(&result, &mut BufReader::new(Cursor::new(bytes)));
        assert_eq!(snapshot, result.snapshot());
    }

    #[test]
    fn concurrent_creation_assigns_each_external_id_once() {
        const NUM_IDS: usize = 10_000;
        const NUM_THREADS: usize = 8;
        let store = IdStore::new();
        let externals: Vec<String> = (0..NUM_IDS).map(|i| format!("id-{i}")).collect();

        thread::scope(|s| {
            for thread_idx in 0..NUM_THREADS {
                let store = &store;
                let externals = &externals;
                s.spawn(move || {
                    // Different threads create the ids in different orders.
                    for i in 0..NUM_IDS {
                        store.create_id::<()>(&externals[(i * (thread_idx + 1)) % NUM_IDS]);
                    }
                    for external in externals {
                        store.create_id::<()>(external);
                    }
                });
            }
        });

        let snapshot = store.snapshot();
        let ids = &snapshot[&<()>::stable_type_id()];
        assert_eq!(NUM_IDS, ids.len());
        for (internal, external) in ids.iter().enumerate() {
            assert_eq!(
                internal as u64,
                store.get_from_ext::<()>(external).internal()
            );
        }
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
