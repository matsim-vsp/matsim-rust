use clap::Parser;
use matsim_rust::simulation::replanning::routing::TransitRoutingModule;
use matsim_rust::simulation::scenario::Coordinate;
use matsim_rust::simulation::scenario::transit::TransitSchedule;
use matsim_rust::simulation::time::SimTime;
use rayon::prelude::*;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

#[derive(Debug, Parser)]
#[command(about = "Create a zone-to-zone PT skim using matsim-rs timetable routing")]
struct Args {
    #[arg(long)]
    schedule: PathBuf,
    #[arg(long)]
    zones: PathBuf,
    #[arg(long)]
    output: PathBuf,
    #[arg(long)]
    departure_time_seconds: u64,
    #[arg(long)]
    walk_speed_mps: f64,
    #[arg(long, default_value_t = 1.0)]
    walk_distance_factor: f64,
    #[arg(long)]
    timing_output: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
struct ZoneConnector {
    zone_id: String,
    x: f64,
    y: f64,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let zones: Vec<_> = csv::Reader::from_path(&args.zones)?
        .deserialize::<ZoneConnector>()
        .map(|row| {
            let row = row?;
            Ok((row.zone_id, Coordinate::new_2d(row.x, row.y)))
        })
        .collect::<Result<_, csv::Error>>()?;
    let schedule = Arc::new(TransitSchedule::from_file(&args.schedule));
    let router = TransitRoutingModule::new_for_skim(
        schedule,
        args.walk_speed_mps,
        args.walk_distance_factor,
    );
    let departure = SimTime::from_secs(args.departure_time_seconds);
    let router = &router;
    let zones_ref = &zones;

    let results: Vec<_> = zones
        .par_iter()
        .map(|(origin_id, origin)| {
            let destinations: Vec<_> = zones_ref
                .iter()
                .filter(|(destination_id, _)| origin_id != destination_id)
                .map(|(_, coordinate)| coordinate.clone())
                .collect();
            let started = Instant::now();
            let results = router.skim_results_from_origin(origin, &destinations, departure);
            let elapsed = started.elapsed().as_secs_f64();
            let rows = zones_ref
                .iter()
                .filter(|(destination_id, _)| origin_id != destination_id)
                .zip(results)
                .map(move |((destination_id, _), result)| {
                    (
                        origin_id,
                        destination_id,
                        result.outcome.as_str(),
                        result.travel_time.map(|time| time.as_secs_f64() / 60.0),
                    )
                })
                .collect::<Vec<_>>();
            (origin_id, elapsed, rows)
        })
        .collect();

    let mut writer = csv::Writer::from_path(&args.output)?;
    writer.write_record(["origin", "destination", "outcome", "travel_time_minutes"])?;
    for (_, _, rows) in &results {
        for (origin, destination, outcome, minutes) in rows {
            writer.write_record([
                origin.as_str(),
                destination.as_str(),
                *outcome,
                &minutes.map_or_else(String::new, |minutes| minutes.to_string()),
            ])?;
        }
    }
    writer.flush()?;
    if let Some(path) = args.timing_output {
        let mut writer = csv::Writer::from_path(path)?;
        writer.write_record(["origin", "search_seconds", "destination_count"])?;
        for (origin, elapsed, rows) in results {
            writer.write_record([origin, &elapsed.to_string(), &rows.len().to_string()])?;
        }
        writer.flush()?;
    }
    Ok(())
}
