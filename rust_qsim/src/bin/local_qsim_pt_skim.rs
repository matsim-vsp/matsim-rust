use clap::Parser;
use rayon::prelude::*;
use rust_qsim::simulation::replanning::routing::TransitRoutingModule;
use rust_qsim::simulation::scenario::Coordinate;
use rust_qsim::simulation::scenario::transit::TransitSchedule;
use rust_qsim::simulation::time::SimTime;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;

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

    let rows: Vec<_> = zones
        .par_iter()
        .flat_map_iter(|(origin_id, origin)| {
            let destinations: Vec<_> = zones_ref
                .iter()
                .map(|(_, coordinate)| coordinate.clone())
                .collect();
            let times = router.skim_times_from_origin(origin, &destinations, departure);
            zones_ref
                .iter()
                .zip(times)
                .filter_map(move |((destination_id, _), time)| {
                    (origin_id != destination_id)
                        .then(|| (origin_id, destination_id, time.as_secs_f64() / 60.0))
                })
        })
        .collect();

    let mut writer = csv::Writer::from_path(&args.output)?;
    writer.write_record(["origin", "destination", "travel_time_minutes"])?;
    for (origin, destination, minutes) in rows {
        writer.write_record([origin, destination, &minutes.to_string()])?;
    }
    writer.flush()?;
    Ok(())
}
