pub mod routing;

use crate::simulation::time::SimTime as SimulationTime;
use arrow2::array::{FixedSizeBinaryArray, UInt64Array};
use std::error::Error;
use std::fmt::Debug;
use std::fs;
use std::fs::File;
use std::io::BufWriter;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime};
use tracing::field::Field;
use tracing::span::Attributes;
use tracing::{Id, Metadata, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::field::Visit;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::{Extensions, LookupSpan};

use arrow2::array::{Array, Int64Array, Utf8Array};
use arrow2::chunk::Chunk;
use arrow2::datatypes::{DataType, Schema};
use arrow2::io::parquet::write::{
    CompressionOptions, Encoding, FileWriter, RowGroupIterator, Version, WriteOptions,
};

pub const BYTE_WIDTH_U128: usize = 16;

// Implementation overview:
// - The layer supports two backends at runtime: CSV and Parquet.
// - The public API exposes constructors `new_csv` and `new_parquet` that return the layer and a WriterGuard.
// - The WriterGuard flushes/writes on drop. CSV uses csv::Writer flush, Parquet writes rows on every on_close using an open FileWriter.

pub struct SpanDurationToFileLayer {
    backend: Backend,
}

#[non_exhaustive]
pub enum WriterGuard {
    Csv(Arc<Mutex<csv::Writer<File>>>),
    Parquet(Arc<Mutex<BufferedSpanData>>),
}

#[non_exhaustive]
enum Backend {
    Csv {
        writer: Arc<Mutex<csv::Writer<File>>>,
    },
    Parquet {
        inner: Arc<Mutex<BufferedSpanData>>,
    },
}

struct SpanDuration {
    elapsed: u64,
    last: Instant,
}

// We need these type wrappers to get distinct types for the extensions
#[derive(Debug)]
struct Uuid(pub u128);
struct PersonId(pub String);
struct Mode(pub String);
struct Rank(pub u64);
struct SimTime(pub SimulationTime);

const MISSING_SIM_TIME: i64 = -1;

struct MetadataVisitor {
    rank: Option<u64>,
    sim_time: Option<SimulationTime>,
}

impl MetadataVisitor {
    fn new() -> Self {
        MetadataVisitor {
            rank: None,
            sim_time: None,
        }
    }
}

impl Visit for MetadataVisitor {
    fn record_u64(&mut self, field: &Field, value: u64) {
        //fetch rank
        if field.name().eq("rank") {
            self.rank = Some(value);
        }

        if let Some(sim_time) = sim_time_from_field(field.name(), value) {
            self.sim_time = Some(sim_time);
        }
    }

    fn record_debug(&mut self, _field: &Field, _value: &dyn Debug) {
        //nothing to do here
    }
}

// Buffered data for parquet backend
pub struct BufferedSpanData {
    // Keep an open parquet FileWriter and schema/options so we can write a row on every on_close
    pub path: std::path::PathBuf,
    schema: Schema,
    options: WriteOptions,
    encodings: Vec<Vec<Encoding>>,
    writer: FileWriter<BufWriter<File>>,

    // buffering fields for batch writes
    batch_size: usize,
    timestamps: Vec<u128>,
    targets: Vec<String>,
    func_names: Vec<String>,
    durations: Vec<u64>,
    sim_times: Vec<i64>,
    ranks: Vec<i64>,
}

impl BufferedSpanData {
    pub fn new(path: std::path::PathBuf, batch_size: usize) -> Self {
        let fields = vec![
            arrow2::datatypes::Field::new(
                "timestamp",
                DataType::FixedSizeBinary(BYTE_WIDTH_U128),
                false,
            ),
            arrow2::datatypes::Field::new("target", DataType::Utf8, false),
            arrow2::datatypes::Field::new("func_name", DataType::Utf8, false),
            arrow2::datatypes::Field::new("duration_ns", DataType::UInt64, false),
            arrow2::datatypes::Field::new("sim_time", DataType::Int64, false),
            arrow2::datatypes::Field::new("rank", DataType::Int64, false),
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
        fs::create_dir_all(prefix).unwrap();

        // open file and create writer
        let file = BufWriter::new(File::create(&path).unwrap());
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
            ranks: Vec::with_capacity(batch_size),
        }
    }

    fn create_parent(&self) {
        let prefix = self.path.parent().unwrap();
        fs::create_dir_all(prefix).unwrap();
    }

    /// Append a single row into the in-memory buffers and flush if we reached batch_size.
    pub fn write_row(
        &mut self,
        timestamp: u128,
        target: &str,
        func_name: &str,
        duration_ns: u64,
        sim_time: i64,
        rank: i64,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.timestamps.push(timestamp);
        self.targets.push(target.to_string());
        self.func_names.push(func_name.to_string());
        self.durations.push(duration_ns);
        self.sim_times.push(sim_time);
        self.ranks.push(rank);

        if self.timestamps.len() >= self.batch_size {
            self.flush_batch()?;
        }

        Ok(())
    }

    /// Convert the accumulated vectors into Arrow arrays and write a single row-group.
    pub fn flush_batch(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        if self.timestamps.is_empty() {
            return Ok(());
        }

        // convert to arrays
        let ts_array = convert_u128_to_fixed_size_binary(&self.timestamps);

        let target_refs: Vec<&str> = self.targets.iter().map(|s| s.as_str()).collect();
        let targets_array = Utf8Array::<i32>::from_slice(&target_refs);

        let func_refs: Vec<&str> = self.func_names.iter().map(|s| s.as_str()).collect();
        let func_array = Utf8Array::<i32>::from_slice(&func_refs);

        let duration_array = UInt64Array::from_slice(&self.durations);
        let sim_time_array = Int64Array::from_slice(&self.sim_times);
        let rank_array = Int64Array::from_slice(&self.ranks);

        let columns: Vec<Box<dyn Array>> = vec![
            Box::new(ts_array),
            Box::new(targets_array),
            Box::new(func_array),
            Box::new(duration_array),
            Box::new(sim_time_array),
            Box::new(rank_array),
        ];

        write_parquet(
            &self.schema,
            columns,
            self.options,
            &self.encodings,
            &mut self.writer,
        )??;

        // clear buffers but keep capacity
        self.timestamps.clear();
        self.targets.clear();
        self.func_names.clear();
        self.durations.clear();
        self.sim_times.clear();
        self.ranks.clear();

        Ok(())
    }

    /// Close the writer (write footer). Call at Drop time. Flush any remaining buffered rows first.
    pub fn close_writer(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        // flush any remaining rows
        self.flush_batch()?;
        // write footer
        self.writer.end(None)?;
        Ok(())
    }
}

impl SpanDurationToFileLayer {
    pub fn new_csv(path: &Path) -> (Self, WriterGuard) {
        let file = create_file(path);
        let mut writer = csv::Writer::from_writer(file);
        writer
            .write_record(vec![
                "timestamp",
                "target",
                "func_name",
                "duration_ns",
                "sim_time",
                "rank",
            ])
            .unwrap();

        // wrap the writer into an arc<mutex<...>> so that we can keep a reference which gets dropped
        // at the end of the scope calling this method. The mutex is necessary, because the Layer
        // must be Sync + Send for the tracing_subscriber subscriber
        let writer_ref = Arc::new(Mutex::new(writer));
        let backend = Backend::Csv {
            writer: writer_ref.clone(),
        };
        (Self { backend }, WriterGuard::Csv(writer_ref))
    }

    pub fn new_parquet(path: &Path, batch_size: usize) -> (Self, WriterGuard) {
        let buf = BufferedSpanData::new(path.to_path_buf(), batch_size);
        buf.create_parent();
        let inner = Arc::new(Mutex::new(buf));
        let backend = Backend::Parquet {
            inner: inner.clone(),
        };
        (Self { backend }, WriterGuard::Parquet(inner))
    }
}

/// Simple Layer implementation, which records the time elapsed between a span being opened and being
/// closed again. Once a span is closed, it writes the elapsed time into a csv journal
///
/// `Context` is managed by the `tracing_subscriber` library. All functions implemented here are called by the
/// `instrument` macro.
///
/// `Attributes` store all custom fields. The `MetadataVisitor` is used to extract the field values.
///
/// `Span` stores information about the scope of an instrumentation call.
impl<S> Layer<S> for SpanDurationToFileLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let span = ctx.span(id).expect("should exist");
        let mut extensions = span.extensions_mut();

        let option = extensions.replace(SpanDuration::new());
        assert!(
            option.is_none(),
            "Trying to initialize Span, but it already exists. This should not happen. \
        It might happen, if multiple Layers are trying to insert the same type into the extensions. \
        Check the configuration of the Layers with respect to their including/excluding module path."
        );

        let mut visitor = MetadataVisitor::new();
        attrs.record(&mut visitor as &mut dyn Visit);
        if let Some(rank) = visitor.rank {
            extensions.insert(Rank(rank));
        }
        if let Some(sim_time) = visitor.sim_time {
            extensions.insert(SimTime(sim_time));
        }
    }

    fn on_enter(&self, id: &Id, ctx: Context<'_, S>) {
        start_timing::<S>(id, ctx);
    }

    fn on_exit(&self, id: &Id, ctx: Context<'_, S>) {
        end_timing::<S>(id, ctx);
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        let span = ctx.span(&id).expect("Span should be there!");
        let extensions = span.extensions();
        let meta = span.metadata();
        let (timestep, target, func_name, duration, sim_time) = extract_entries(&extensions, meta);
        let rank = extensions.get::<Rank>().map_or(-1, |rank| rank.0 as i64);
        match &self.backend {
            Backend::Csv { writer, .. } => {
                let writer = &mut *writer.lock().unwrap();
                writer
                    .write_record([
                        &timestep.to_string(),
                        target,
                        func_name,
                        &duration.to_string(),
                        &sim_time.to_string(),
                        &rank.to_string(),
                    ])
                    .unwrap();

                drop(extensions);
                drop(span);
            }
            Backend::Parquet { inner, .. } => {
                let mut inner = inner.lock().unwrap();
                // write a single row immediately
                if let Err(e) =
                    inner.write_row(timestep, target, func_name, duration, sim_time, rank)
                {
                    eprintln!("Failed to write parquet row: {}", e);
                }

                drop(extensions);
                drop(span);
            }
        }
    }
}

fn extract_entries<'a>(
    extensions: &Extensions,
    meta: &Metadata<'a>,
) -> (u128, &'a str, &'a str, u64, i64) {
    let timestep = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let target = meta.target();
    let func_name = meta.name();
    let span_duration = extensions.get::<SpanDuration>().unwrap().elapsed;
    let sim_time = extensions
        .get::<SimTime>()
        .map_or(MISSING_SIM_TIME, |sim_time| {
            sim_time_to_output_seconds(sim_time.0)
        });
    (timestep, target, func_name, span_duration, sim_time)
}

/// `tracing` records `SimTime` arguments via `Debug`, which is great for human-readable spans but
/// awkward to parse back in a profiling layer. Instrumented `SimTime` spans therefore emit a
/// parallel `*_ns` scalar field that we can losslessly reconstruct into `SimTime` here.
pub(crate) fn sim_time_from_field(field_name: &str, value: u64) -> Option<SimulationTime> {
    if matches!(field_name, "now_ns" | "sim_time_ns" | "sim_time") || field_name.contains("now") {
        Some(SimulationTime::from_nanos(value))
    } else {
        None
    }
}

fn sim_time_to_output_seconds(sim_time: SimulationTime) -> i64 {
    sim_time.as_secs().min(i64::MAX as u64) as i64
}

fn end_timing<S: Subscriber + for<'a> LookupSpan<'a>>(id: &Id, ctx: Context<S>) {
    let span = ctx.span(id).expect("Span should be there");
    let mut extensions = span.extensions_mut();

    if let Some(timing) = extensions.get_mut::<SpanDuration>() {
        let now = Instant::now();
        timing.elapsed += (now - timing.last).as_nanos() as u64;
    }
}

/// Start timing for span
fn start_timing<S: Subscriber + for<'a> LookupSpan<'a>>(id: &Id, ctx: Context<S>) {
    let span = ctx.span(id).expect("Should exist");
    let mut extensions = span.extensions_mut();

    if let Some(timing) = extensions.get_mut::<SpanDuration>() {
        timing.last = Instant::now();
    }
}

pub fn create_file(path: &Path) -> File {
    let prefix = path.parent().unwrap();
    fs::create_dir_all(prefix).unwrap();
    File::create(path).unwrap_or_else(|_e| panic!("Failed to open file at: {path:?}"))
}

fn convert_u128_to_fixed_size_binary(data: &Vec<u128>) -> FixedSizeBinaryArray {
    let mut timestamp_data = Vec::with_capacity(data.len() * BYTE_WIDTH_U128);
    for t in data {
        timestamp_data.extend_from_slice(&t.to_le_bytes());
    }
    FixedSizeBinaryArray::new(
        DataType::FixedSizeBinary(BYTE_WIDTH_U128),
        timestamp_data.into(),
        None,
    )
}

// Keep write_parquet helper for compatibility but not used for per-row writing
fn write_parquet(
    schema: &Schema,
    columns: Vec<Box<dyn Array>>,
    options: WriteOptions,
    encodings: &[Vec<Encoding>],
    writer: &mut FileWriter<std::io::BufWriter<File>>,
) -> Result<Result<(), Box<dyn Error>>, Box<dyn Error>> {
    let chunk = Chunk::new(columns);
    let row_group_iter = RowGroupIterator::try_new(
        vec![Ok(chunk)].into_iter(),
        schema,
        options,
        Vec::from(encodings),
    )?;
    for rg in row_group_iter {
        let rg = rg?;
        writer.write(rg)?;
    }
    Ok(Ok(()))
}

impl Drop for WriterGuard {
    fn drop(&mut self) {
        match self {
            WriterGuard::Csv(writer) => {
                let mut writer = writer.lock().unwrap();
                writer.flush().expect("Problem flushing writer");
            }
            WriterGuard::Parquet(inner) => {
                let mut guard = inner.lock().unwrap();
                if let Err(e) = guard.close_writer() {
                    eprintln!("Failed to close parquet writer: {}", e);
                }
            }
        }
    }
}

impl SpanDuration {
    fn new() -> Self {
        Self {
            elapsed: 0,
            last: Instant::now(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::thread::sleep;
    use std::time::Duration;

    use crate::simulation::time::SimTime;
    use tracing::level_filters::LevelFilter;
    use tracing::{info, instrument};
    use tracing_subscriber::Layer as OtherLayer;
    use tracing_subscriber::fmt::Layer;
    use tracing_subscriber::fmt::format::FmtSpan;
    use tracing_subscriber::layer::SubscriberExt;

    use crate::simulation::profiling::SpanDurationToFileLayer;

    #[test]
    fn test_events() {
        let path = PathBuf::from("./test_output/simulation/profiling/test_events.csv");

        let (csv_layer, _guard) = SpanDurationToFileLayer::new_csv(&path);
        let layers = tracing_subscriber::registry().with(csv_layer).with(
            Layer::new()
                .with_span_events(FmtSpan::CLOSE)
                .with_filter(LevelFilter::TRACE),
        );
        tracing::subscriber::set_global_default(layers).expect("TODO: panic message");

        info!("Before func");
        some_function();
        info!("After func");

        some_other_function(SimTime::from_secs(7), std::f32::consts::PI);
    }

    #[instrument]
    fn some_function() {
        info!("Inside some function.")
    }

    #[instrument(
        level = "trace",
        fields(
            rank = 42u32,
            now_ns = now.as_nanos()
        )
    )]
    fn some_other_function(now: SimTime, b: f32) {
        info!("Inside some other function");
        sleep(Duration::from_nanos(10));
    }
}
