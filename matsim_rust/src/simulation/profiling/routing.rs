use crate::simulation::profiling::{
    BYTE_WIDTH_U128, Mode, PersonId, SimTime, SpanDuration, Uuid,
    convert_u128_to_fixed_size_binary, create_file, end_timing, extract_entries,
    sim_time_from_field, start_timing, write_parquet,
};
use std::fmt::Debug;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tracing::field::{Field, Visit};
use tracing::span::Attributes;
use tracing::{Event, Id};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;

use arrow2::array::{Array, Int64Array, UInt64Array, Utf8Array};
use arrow2::datatypes::{DataType, Schema};
use arrow2::io::parquet::write::{CompressionOptions, Encoding, FileWriter, Version, WriteOptions};
use std::fs::File;

const HEADER: [&str; 8] = [
    "timestamp",
    "target",
    "func_name",
    "duration_ns",
    "sim_time",
    "request_uuid",
    "person_id",
    "mode",
];

pub enum RoutingWriterGuard {
    Csv(Arc<Mutex<csv::Writer<File>>>),
    Parquet(Arc<Mutex<BufferedRoutingData>>),
}

pub enum RoutingBackend {
    Csv {
        writer: Arc<Mutex<csv::Writer<File>>>,
    },
    Parquet {
        inner: Arc<Mutex<BufferedRoutingData>>,
    },
}

pub struct RoutingSpanDurationToFileLayer {
    backend: RoutingBackend,
}

impl RoutingSpanDurationToFileLayer {
    pub fn new_csv(path: &Path) -> (Self, RoutingWriterGuard) {
        let file = create_file(path);
        let mut raw_writer = csv::Writer::from_writer(file);
        raw_writer.write_record(HEADER).unwrap();
        let writer = Arc::new(Mutex::new(raw_writer));
        (
            Self {
                backend: RoutingBackend::Csv {
                    writer: writer.clone(),
                },
            },
            RoutingWriterGuard::Csv(writer),
        )
    }

    pub fn new_parquet(path: &Path, batch_size: usize) -> (Self, RoutingWriterGuard) {
        let buf = BufferedRoutingData::new(path.to_path_buf(), batch_size);
        buf.create_parent();
        let inner = Arc::new(Mutex::new(buf));
        (
            Self {
                backend: RoutingBackend::Parquet {
                    inner: inner.clone(),
                },
            },
            RoutingWriterGuard::Parquet(inner),
        )
    }
}

impl<S> Layer<S> for RoutingSpanDurationToFileLayer
where
    S: tracing::Subscriber + for<'lookup> tracing_subscriber::registry::LookupSpan<'lookup>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let span = ctx.span(id).expect("should exist");
        let mut extensions = span.extensions_mut();
        extensions.insert(SpanDuration::new());

        let mut visitor = RoutingMetadataVisitor::default();
        attrs.record(&mut visitor as &mut dyn Visit);

        if let Some(sim_time) = visitor.sim_time {
            extensions.insert(sim_time);
        }
        if let Some(uuid) = visitor.uuid {
            extensions.insert(uuid);
        }
        if let Some(person_id) = visitor.person_id {
            extensions.insert(person_id);
        }
        if let Some(mode) = visitor.mode {
            extensions.insert(mode);
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        if let Some(id) = ctx.current_span().id() {
            let span = ctx.span(id).expect("Span should be there!");
            let span_target = span.metadata().target();
            let module = span_target == event.metadata().target();

            if !module {
                return;
            }

            let mut visitor = RoutingMetadataVisitor::default();
            event.record(&mut visitor);

            let mut exts = span.extensions_mut();
            if let Some(uuid) = visitor.uuid {
                let v = exts.replace(uuid);
                assert!(
                    v.is_none(),
                    "Uuid already present in span; unexpected duplicate event registration. Event: {:?}",
                    event
                );
            }

            if let Some(mode) = visitor.mode {
                let v = exts.replace(mode);
                assert!(
                    v.is_none(),
                    "Mode already present in span; unexpected duplicate event registration. Event: {:?}",
                    event
                );
            }
        }
    }

    fn on_enter(&self, id: &Id, ctx: Context<'_, S>) {
        start_timing(id, ctx);
    }

    fn on_exit(&self, id: &Id, ctx: Context<'_, S>) {
        end_timing(id, ctx);
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        match &self.backend {
            RoutingBackend::Csv { writer } => {
                let writer = &mut *writer.lock().unwrap();

                let span = ctx.span(&id).expect("Span should be there!");
                let extensions = span.extensions();
                let meta = span.metadata();

                let (timestep, target, func_name, duration, sim_time) =
                    extract_entries(&extensions, meta);
                let request_uuid = extensions
                    .get::<Uuid>()
                    .map_or("-1".to_string(), |uuid| uuid.0.to_string());
                let person_id = extensions
                    .get::<PersonId>()
                    .map_or("", |person_id| person_id.0.as_str());
                let mode = extensions.get::<Mode>().map_or("", |mode| mode.0.as_str());

                writer
                    .write_record([
                        &timestep.to_string(),
                        target,
                        func_name,
                        &duration.to_string(),
                        &sim_time.to_string(),
                        &request_uuid,
                        person_id,
                        mode,
                    ])
                    .unwrap_or_else(|e| panic!("Failed to write record. {}", e));

                drop(extensions);
                drop(span);
            }
            RoutingBackend::Parquet { inner } => {
                let span = ctx.span(&id).expect("Span should be there!");
                let extensions = span.extensions();
                let meta = span.metadata();

                let (timestep, _target, func_name, duration, sim_time) =
                    extract_entries(&extensions, meta);
                let request_uuid = extensions.get::<Uuid>().map_or(0, |uuid| uuid.0);
                let person_id = extensions
                    .get::<PersonId>()
                    .map_or("".to_string(), |person_id| person_id.0.clone());
                let mode = extensions
                    .get::<Mode>()
                    .map_or("".to_string(), |mode| mode.0.clone());

                let mut inner = inner.lock().unwrap();
                if let Err(e) = inner.write_row(
                    timestep,
                    meta.target(),
                    func_name,
                    duration,
                    sim_time,
                    request_uuid,
                    person_id.as_str(),
                    mode.as_str(),
                ) {
                    eprintln!("Failed to write routing parquet row: {}", e);
                }

                drop(extensions);
                drop(span);
            }
        }
    }
}

// Parquet writer for routing data – writes rows immediately.
pub struct BufferedRoutingData {
    pub path: std::path::PathBuf,
    schema: Schema,
    options: WriteOptions,
    encodings: Vec<Vec<Encoding>>,
    writer: FileWriter<std::io::BufWriter<File>>,
    batch_size: usize,
    // for buffering rows before writing to parquet file
    timestamps: Vec<u128>,
    targets: Vec<String>,
    func_names: Vec<String>,
    durations: Vec<u64>,
    sim_times: Vec<i64>,
    request_uuids: Vec<u128>,
    person_ids: Vec<String>,
    modes: Vec<String>,
}

impl BufferedRoutingData {
    pub fn new(path: std::path::PathBuf, batch_size: usize) -> Self {
        let fields = vec![
            arrow2::datatypes::Field::new(
                "timestamp",
                DataType::FixedSizeBinary(BYTE_WIDTH_U128),
                false,
            ),
            arrow2::datatypes::Field::new("target", DataType::Utf8, false),
            arrow2::datatypes::Field::new("func_name", DataType::Utf8, false),
            arrow2::datatypes::Field::new("duration_ns", DataType::Int64, false),
            arrow2::datatypes::Field::new("sim_time", DataType::Int64, false),
            arrow2::datatypes::Field::new(
                "request_uuid",
                DataType::FixedSizeBinary(BYTE_WIDTH_U128),
                false,
            ),
            arrow2::datatypes::Field::new("person_id", DataType::Utf8, false),
            arrow2::datatypes::Field::new("mode", DataType::Utf8, false),
        ];
        let schema = Schema::from(fields);

        let options = WriteOptions {
            write_statistics: false,
            version: Version::V2,
            compression: CompressionOptions::Snappy,
            data_pagesize_limit: None,
        };

        let encodings = vec![vec![Encoding::Plain]; schema.fields.len()];

        // create parent dirs
        let prefix = path.parent().unwrap();
        std::fs::create_dir_all(prefix).unwrap();

        let file = std::io::BufWriter::new(File::create(&path).unwrap());
        let writer = FileWriter::try_new(file, schema.clone(), options)
            .expect("Failed to create parquet FileWriter");

        Self {
            path,
            schema,
            options,
            encodings,
            writer,
            batch_size,
            timestamps: Vec::with_capacity(batch_size),
            targets: Vec::with_capacity(batch_size),
            func_names: Vec::with_capacity(batch_size),
            durations: Vec::with_capacity(batch_size),
            sim_times: Vec::with_capacity(batch_size),
            request_uuids: Vec::with_capacity(batch_size),
            person_ids: Vec::with_capacity(batch_size),
            modes: Vec::with_capacity(batch_size),
        }
    }

    fn create_parent(&self) {
        let prefix = self.path.parent().unwrap();
        std::fs::create_dir_all(prefix).unwrap();
    }

    #[allow(clippy::too_many_arguments)]
    pub fn write_row(
        &mut self,
        timestamp: u128,
        target: &str,
        func_name: &str,
        duration_ns: u64,
        sim_time: i64,
        request_uuid: u128,
        person_id: &str,
        mode: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.timestamps.push(timestamp);
        self.targets.push(target.to_string());
        self.func_names.push(func_name.to_string());
        self.durations.push(duration_ns);
        self.sim_times.push(sim_time);
        self.request_uuids.push(request_uuid);
        self.person_ids.push(person_id.to_string());
        self.modes.push(mode.to_string());

        if self.timestamps.len() >= self.batch_size {
            self.flush_batch()?;
        }

        Ok(())
    }

    pub fn flush_batch(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        if self.timestamps.is_empty() {
            return Ok(());
        }

        let ts_array = convert_u128_to_fixed_size_binary(&self.timestamps);

        let target_refs: Vec<&str> = self.targets.iter().map(|s| s.as_str()).collect();
        let targets_array = Utf8Array::<i32>::from_slice(&target_refs);

        let func_refs: Vec<&str> = self.func_names.iter().map(|s| s.as_str()).collect();
        let func_array = Utf8Array::<i32>::from_slice(&func_refs);

        let duration_array = UInt64Array::from_slice(&self.durations);
        let sim_time_array = Int64Array::from_slice(&self.sim_times);

        let req_uuid_array = convert_u128_to_fixed_size_binary(&self.request_uuids);

        let person_refs: Vec<&str> = self.person_ids.iter().map(|s| s.as_str()).collect();
        let person_array = Utf8Array::<i32>::from_slice(&person_refs);

        let mode_refs: Vec<&str> = self.modes.iter().map(|s| s.as_str()).collect();
        let mode_array = Utf8Array::<i32>::from_slice(&mode_refs);

        let columns: Vec<Box<dyn Array>> = vec![
            Box::new(ts_array),
            Box::new(targets_array),
            Box::new(func_array),
            Box::new(duration_array),
            Box::new(sim_time_array),
            Box::new(req_uuid_array),
            Box::new(person_array),
            Box::new(mode_array),
        ];

        write_parquet(
            &self.schema,
            columns,
            self.options,
            &self.encodings,
            &mut self.writer,
        )??;

        self.timestamps.clear();
        self.targets.clear();
        self.func_names.clear();
        self.durations.clear();
        self.sim_times.clear();
        self.request_uuids.clear();
        self.person_ids.clear();
        self.modes.clear();

        Ok(())
    }

    pub fn close_writer(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.flush_batch()?;
        self.writer.end(None)?;
        Ok(())
    }
}

#[derive(Default)]
struct RoutingMetadataVisitor {
    sim_time: Option<SimTime>,
    uuid: Option<Uuid>,
    person_id: Option<PersonId>,
    mode: Option<Mode>,
}

impl Visit for RoutingMetadataVisitor {
    fn record_u64(&mut self, field: &Field, value: u64) {
        if let Some(sim_time) = sim_time_from_field(field.name(), value) {
            self.sim_time = Some(SimTime(sim_time));
        }
    }

    fn record_u128(&mut self, field: &Field, value: u128) {
        if field.name().eq("uuid") {
            self.uuid = Some(Uuid(value));
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name().eq("person_id") {
            self.person_id = Some(PersonId(value.to_string()));
        }
        if field.name().eq("mode") {
            self.mode = Some(Mode(value.to_string()));
        }
    }

    fn record_debug(&mut self, _field: &Field, _value: &dyn Debug) {}
}

impl Drop for RoutingWriterGuard {
    fn drop(&mut self) {
        match self {
            RoutingWriterGuard::Csv(writer) => {
                let mut writer = writer.lock().unwrap();
                writer.flush().expect("Problem flushing writer");
            }
            RoutingWriterGuard::Parquet(inner) => {
                let mut inner = inner.lock().unwrap();
                if let Err(e) = inner.close_writer() {
                    eprintln!("Failed to close routing parquet profiling file: {}", e);
                }
            }
        }
    }
}

// tests are integration tests as they require exclusive execution
