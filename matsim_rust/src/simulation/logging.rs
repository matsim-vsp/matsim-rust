use std::io;
use std::path::Path;
use tracing::dispatcher::DefaultGuard;
use tracing::{Level, info};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::{non_blocking, rolling};
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::fmt::format::FmtSpan;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::{EnvFilter, Layer};
use tracing_subscriber::{fmt, registry};

use crate::simulation::build_info::GIT_VERSION;
use crate::simulation::config::{Config, Logging, Profiling};
use crate::simulation::io::resolve_path;
use crate::simulation::profiling::SpanDurationToFileLayer;
use crate::simulation::profiling::routing::RoutingSpanDurationToFileLayer;

// This is a helper struct to store the logger guards. When they are dropped, logging can be reset.
#[allow(dead_code)]
pub(crate) struct LogGuards {
    tracing_guards: Vec<Box<dyn std::any::Any + Send>>,
    log_guard: Option<WorkerGuard>,
    default: DefaultGuard,
}

#[must_use = "dropping the guard immediately disables logging; bind it, e.g. `let _guard = ...`"]
pub fn init_std_out_logging_thread_local() -> DefaultGuard {
    let collector = tracing_subscriber::registry().with(
        fmt::Layer::new()
            .with_writer(io::stdout)
            .with_filter(LevelFilter::INFO),
    );
    let guard = tracing::subscriber::set_default(collector);
    // Log the code state first, so that every log documents which build produced it.
    info!("matsim-rust version {GIT_VERSION}");
    guard
}

pub(crate) fn init_logging(config: &Config, part: u32) -> LogGuards {
    init_logging_for(config, &part.to_string(), part == 0)
}

/// Logging is thread-local. The controller thread runs scoring and replanning, so it needs its own
/// subscriber to write their spans into the instrument files.
pub(crate) fn init_controller_logging(config: &Config) -> LogGuards {
    init_logging_for(config, "controller", true)
}

fn init_logging_for(config: &Config, file_discriminant: &str, primary: bool) -> LogGuards {
    let dir = resolve_path(config.context(), &config.output().output_dir);

    let csv_layers = init_tracing(config, primary, file_discriminant, &dir);
    let (log_layer, log_guard) = if Logging::Info == config.output().logging {
        let log_file_name = format!("logs/log_process_{file_discriminant}.txt");
        let log_file_appender = rolling::never(&dir, log_file_name);
        let (log_file, log_guard) = non_blocking(log_file_appender);
        let layer = fmt::Layer::new()
            .with_writer(log_file)
            .json()
            .with_ansi(false)
            .with_filter(LevelFilter::INFO);
        (Some(layer), Some(log_guard))
    } else {
        (None, None)
    };

    let console_layer = primary.then(|| {
        fmt::layer()
            .with_writer(io::stdout)
            .with_span_events(FmtSpan::CLOSE)
            .with_filter(LevelFilter::INFO)
    });

    // Add `Optional`s. If None, then the corresponding layer is not added.
    let collector = registry()
        .with(log_layer)
        .with(console_layer)
        // This seems to be a bit odd, but is necessary to let the compiler figure out the correct types.
        // Depending on the order of the layers, the types of the input values are different.
        // This is why the layers are plugged together here and not stored in the csv_layers struct.
        .with(csv_layers.general.map(|(s, e)| s.with_filter(e)))
        .with(csv_layers.routing.map(|(s, e)| s.with_filter(e)));

    let default = tracing::subscriber::set_default(collector);
    // Log the code state first, so that every log documents which build produced it.
    info!("matsim-rust version {GIT_VERSION}");

    LogGuards {
        tracing_guards: csv_layers.writer_guards,
        log_guard,
        default,
    }
}

fn init_tracing(config: &Config, primary: bool, file_discriminant: &str, dir: &Path) -> FileLayers {
    // if we set profiling at all and if profiling is set to level trace, then each process creates an instrumenting file
    // if profiling level is set to INFO, only process 0 and the controller create an instrument file. This is important if we run on a lot of
    // processes, because then we spend a lot of computing time on creating instrument files for each process.
    let instrument_dir = dir.join("instrument");
    let duration_file_name = format!("instrument_process_{file_discriminant}");
    let mut duration_path = instrument_dir.join(duration_file_name);

    let routing_file_name = format!("routing_process_{file_discriminant}");
    let mut routing_path = instrument_dir.join(routing_file_name);

    match &config.output().profiling {
        Profiling::CSV(level_string) => {
            let level = level_string.create_tracing_level();
            if level.eq(&Level::INFO) && primary || level.eq(&Level::TRACE) {
                duration_path.set_extension("csv");
                routing_path.set_extension("csv");
                let (general, general_guard) = SpanDurationToFileLayer::new_csv(&duration_path);
                let (routing, routing_guard) =
                    RoutingSpanDurationToFileLayer::new_csv(&routing_path);
                let (routing_filter, general_filter) = create_filter(level);

                FileLayers {
                    general: Some((general, general_filter)),
                    routing: Some((routing, routing_filter)),
                    writer_guards: vec![Box::new(general_guard), Box::new(routing_guard)],
                }
            } else {
                FileLayers::new()
            }
        }
        Profiling::Parquet(p) => {
            let level = p.create_tracing_level();
            if level.eq(&Level::INFO) && primary || level.eq(&Level::TRACE) {
                duration_path.set_extension("parquet");
                routing_path.set_extension("parquet");
                let (general, general_guard) =
                    SpanDurationToFileLayer::new_parquet(&duration_path, p.batch_size);
                let (routing, routing_guard) =
                    RoutingSpanDurationToFileLayer::new_parquet(&routing_path, p.batch_size);
                let (routing_filter, general_filter) = create_filter(level);

                FileLayers {
                    general: Some((general, general_filter)),
                    routing: Some((routing, routing_filter)),
                    writer_guards: vec![Box::new(general_guard), Box::new(routing_guard)],
                }
            } else {
                FileLayers::new()
            }
        }
        _ => FileLayers::new(),
    }
}

fn create_filter(level: Level) -> (EnvFilter, EnvFilter) {
    let routing_mod = "matsim_rust::simulation::agents::agent_logic";
    let routing_filter = EnvFilter::new(format!("{}={}", routing_mod, level));
    let general_filter = EnvFilter::new(format!("{},{}=off", level, routing_mod));
    (routing_filter, general_filter)
}

#[derive(Default)]
struct FileLayers {
    general: Option<(SpanDurationToFileLayer, EnvFilter)>,
    routing: Option<(RoutingSpanDurationToFileLayer, EnvFilter)>,
    writer_guards: Vec<Box<dyn std::any::Any + Send>>,
}

impl FileLayers {
    pub fn new() -> Self {
        Self {
            general: None,
            routing: None,
            writer_guards: Vec::new(),
        }
    }
}
