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

const HEADER: [&str; 15] = [
    "timestamp",
    "target",
    "func_name",
    "duration_ns",
    "sim_time",
    "request_uuid",
    "person_id",
    "mode",
    "node_count",
    "nodes_expanded",
    "cache_hit",
    "candidate_valid",
    "candidate_bound_used",
    "candidate_validation_ns",
    "fallback_search",
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
        extensions.insert(SearchMetrics {
            node_count: visitor.node_count,
            nodes_expanded: visitor.nodes_expanded,
            cache_hit: visitor.cache_hit,
            candidate_valid: visitor.candidate_valid,
            candidate_bound_used: visitor.candidate_bound_used,
            candidate_validation_ns: visitor.candidate_validation_ns,
            fallback_search: visitor.fallback_search,
        });
    }

    fn on_record(&self, id: &Id, values: &tracing::span::Record<'_>, ctx: Context<'_, S>) {
        let span = ctx.span(id).expect("Span should exist");
        let mut visitor = RoutingMetadataVisitor::default();
        values.record(&mut visitor);
        let mut extensions = span.extensions_mut();
        let metrics = extensions
            .get_mut::<SearchMetrics>()
            .expect("Search metrics are initialized when the span is created");
        if visitor.node_count.is_some() {
            metrics.node_count = visitor.node_count;
        }
        if visitor.nodes_expanded.is_some() {
            metrics.nodes_expanded = visitor.nodes_expanded;
        }
        if visitor.cache_hit.is_some() {
            metrics.cache_hit = visitor.cache_hit;
        }
        if visitor.candidate_valid.is_some() {
            metrics.candidate_valid = visitor.candidate_valid;
        }
        if visitor.candidate_bound_used.is_some() {
            metrics.candidate_bound_used = visitor.candidate_bound_used;
        }
        if visitor.candidate_validation_ns.is_some() {
            metrics.candidate_validation_ns = visitor.candidate_validation_ns;
        }
        if visitor.fallback_search.is_some() {
            metrics.fallback_search = visitor.fallback_search;
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
                let metrics = extensions.get::<SearchMetrics>().unwrap();
                let node_count = metrics
                    .node_count
                    .map(|n| n.to_string())
                    .unwrap_or_default();
                let nodes_expanded = metrics
                    .nodes_expanded
                    .map(|n| n.to_string())
                    .unwrap_or_default();
                let cache_hit = metrics.cache_hit.unwrap_or(false).to_string();
                let candidate_valid = metrics.candidate_valid.unwrap_or(false).to_string();
                let candidate_bound_used =
                    metrics.candidate_bound_used.unwrap_or(false).to_string();
                let candidate_validation_ns = metrics
                    .candidate_validation_ns
                    .unwrap_or_default()
                    .to_string();
                let fallback_search = metrics.fallback_search.unwrap_or(false).to_string();

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
                        &node_count,
                        &nodes_expanded,
                        &cache_hit,
                        &candidate_valid,
                        &candidate_bound_used,
                        &candidate_validation_ns,
                        &fallback_search,
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
                let metrics = extensions.get::<SearchMetrics>().unwrap();

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
                    metrics.node_count.map_or(-1, |value| value as i64),
                    metrics.nodes_expanded.map_or(-1, |value| value as i64),
                    metrics.cache_hit.unwrap_or(false),
                    metrics.candidate_valid.unwrap_or(false),
                    metrics.candidate_bound_used.unwrap_or(false),
                    metrics.candidate_validation_ns.unwrap_or_default(),
                    metrics.fallback_search.unwrap_or(false),
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
    node_counts: Vec<i64>,
    nodes_expanded: Vec<i64>,
    cache_hits: Vec<bool>,
    candidates_valid: Vec<bool>,
    candidate_bound_useds: Vec<bool>,
    candidate_validation_nanos: Vec<u64>,
    fallback_searches: Vec<bool>,
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
            arrow2::datatypes::Field::new("node_count", DataType::Int64, false),
            arrow2::datatypes::Field::new("nodes_expanded", DataType::Int64, false),
            arrow2::datatypes::Field::new("cache_hit", DataType::Boolean, false),
            arrow2::datatypes::Field::new("candidate_valid", DataType::Boolean, false),
            arrow2::datatypes::Field::new("candidate_bound_used", DataType::Boolean, false),
            arrow2::datatypes::Field::new("candidate_validation_ns", DataType::UInt64, false),
            arrow2::datatypes::Field::new("fallback_search", DataType::Boolean, false),
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
            node_counts: Vec::with_capacity(batch_size),
            nodes_expanded: Vec::with_capacity(batch_size),
            cache_hits: Vec::with_capacity(batch_size),
            candidates_valid: Vec::with_capacity(batch_size),
            candidate_bound_useds: Vec::with_capacity(batch_size),
            candidate_validation_nanos: Vec::with_capacity(batch_size),
            fallback_searches: Vec::with_capacity(batch_size),
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
        node_count: i64,
        nodes_expanded: i64,
        cache_hit: bool,
        candidate_valid: bool,
        candidate_bound_used: bool,
        candidate_validation_ns: u64,
        fallback_search: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.timestamps.push(timestamp);
        self.targets.push(target.to_string());
        self.func_names.push(func_name.to_string());
        self.durations.push(duration_ns);
        self.sim_times.push(sim_time);
        self.request_uuids.push(request_uuid);
        self.person_ids.push(person_id.to_string());
        self.modes.push(mode.to_string());
        self.node_counts.push(node_count);
        self.nodes_expanded.push(nodes_expanded);
        self.cache_hits.push(cache_hit);
        self.candidates_valid.push(candidate_valid);
        self.candidate_bound_useds.push(candidate_bound_used);
        self.candidate_validation_nanos
            .push(candidate_validation_ns);
        self.fallback_searches.push(fallback_search);

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
        let node_count_array = Int64Array::from_slice(&self.node_counts);
        let nodes_expanded_array = Int64Array::from_slice(&self.nodes_expanded);
        let cache_hits_array = arrow2::array::BooleanArray::from_slice(&self.cache_hits);
        let candidates_valid_array =
            arrow2::array::BooleanArray::from_slice(&self.candidates_valid);
        let candidate_bound_useds_array =
            arrow2::array::BooleanArray::from_slice(&self.candidate_bound_useds);
        let candidate_validation_nanos_array =
            UInt64Array::from_slice(&self.candidate_validation_nanos);
        let fallback_searches_array =
            arrow2::array::BooleanArray::from_slice(&self.fallback_searches);

        let columns: Vec<Box<dyn Array>> = vec![
            Box::new(ts_array),
            Box::new(targets_array),
            Box::new(func_array),
            Box::new(duration_array),
            Box::new(sim_time_array),
            Box::new(req_uuid_array),
            Box::new(person_array),
            Box::new(mode_array),
            Box::new(node_count_array),
            Box::new(nodes_expanded_array),
            Box::new(cache_hits_array),
            Box::new(candidates_valid_array),
            Box::new(candidate_bound_useds_array),
            Box::new(candidate_validation_nanos_array),
            Box::new(fallback_searches_array),
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
        self.node_counts.clear();
        self.nodes_expanded.clear();
        self.cache_hits.clear();
        self.candidates_valid.clear();
        self.candidate_bound_useds.clear();
        self.candidate_validation_nanos.clear();
        self.fallback_searches.clear();

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
    node_count: Option<u64>,
    nodes_expanded: Option<u64>,
    cache_hit: Option<bool>,
    candidate_valid: Option<bool>,
    candidate_bound_used: Option<bool>,
    candidate_validation_ns: Option<u64>,
    fallback_search: Option<bool>,
}

#[derive(Default)]
struct SearchMetrics {
    node_count: Option<u64>,
    nodes_expanded: Option<u64>,
    cache_hit: Option<bool>,
    candidate_valid: Option<bool>,
    candidate_bound_used: Option<bool>,
    candidate_validation_ns: Option<u64>,
    fallback_search: Option<bool>,
}

impl Visit for RoutingMetadataVisitor {
    fn record_u64(&mut self, field: &Field, value: u64) {
        match field.name() {
            "node_count" => self.node_count = Some(value),
            "nodes_expanded" => self.nodes_expanded = Some(value),
            "candidate_validation_ns" => self.candidate_validation_ns = Some(value),
            _ => {}
        }
        if let Some(sim_time) = sim_time_from_field(field.name(), value) {
            self.sim_time = Some(SimTime(sim_time));
        }
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        match field.name() {
            "cache_hit" => self.cache_hit = Some(value),
            "candidate_valid" => self.candidate_valid = Some(value),
            "candidate_bound_used" => self.candidate_bound_used = Some(value),
            "fallback_search" => self.fallback_search = Some(value),
            _ => {}
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

#[cfg(test)]
mod tests {
    use super::RoutingSpanDurationToFileLayer;
    use tracing_subscriber::prelude::*;

    #[test]
    fn csv_records_a_star_search_counts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("routing.csv");
        let (layer, guard) = RoutingSpanDurationToFileLayer::new_csv(&path);
        let subscriber = tracing_subscriber::registry().with(layer);

        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!(
                target: "matsim_rust::simulation::replanning::routing::a_star",
                "least_cost_path_search",
                node_count = 42_u64,
                nodes_expanded = tracing::field::Empty,
                cache_hit = false,
                candidate_valid = tracing::field::Empty,
                candidate_bound_used = tracing::field::Empty,
                candidate_validation_ns = tracing::field::Empty,
                fallback_search = tracing::field::Empty,
            );
            let _entered = span.enter();
            span.record("nodes_expanded", 7_u64);
            span.record("candidate_valid", true);
            span.record("candidate_validation_ns", 15_u64);
            span.record("fallback_search", true);
        });

        drop(guard);
        let mut reader = csv::Reader::from_path(path).unwrap();
        let headers = reader.headers().unwrap().clone();
        let row = reader.records().next().unwrap().unwrap();
        let node_count = headers
            .iter()
            .position(|name| name == "node_count")
            .unwrap();
        let expanded = headers
            .iter()
            .position(|name| name == "nodes_expanded")
            .unwrap();
        let candidate_valid = headers
            .iter()
            .position(|name| name == "candidate_valid")
            .unwrap();
        let validation_ns = headers
            .iter()
            .position(|name| name == "candidate_validation_ns")
            .unwrap();
        let fallback = headers
            .iter()
            .position(|name| name == "fallback_search")
            .unwrap();
        assert_eq!(&row[node_count], "42");
        assert_eq!(&row[expanded], "7");
        assert_eq!(&row[candidate_valid], "true");
        assert_eq!(&row[validation_ns], "15");
        assert_eq!(&row[fallback], "true");
    }

    #[test]
    fn parquet_writes_route_search_metrics() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("routing.parquet");
        let (layer, guard) = RoutingSpanDurationToFileLayer::new_parquet(&path, 1);
        let subscriber = tracing_subscriber::registry().with(layer);

        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!(
                target: "matsim_rust::simulation::replanning::routing::a_star",
                "least_cost_path_search",
                node_count = 42_u64,
                nodes_expanded = 7_u64,
                cache_hit = false,
                candidate_valid = true,
                candidate_bound_used = true,
                candidate_validation_ns = 15_u64,
                fallback_search = false,
            );
            let _entered = span.enter();
        });

        drop(guard);
        assert!(std::fs::metadata(path).unwrap().len() > 0);
    }
}

// tests are integration tests as they require exclusive execution
