use crate::simulation::network::link::CarSpotSpeedSample;
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io;
use std::path::PathBuf;

const MAX_SPEED_KMH: f64 = 120.0;

#[derive(Default)]
struct Cell {
    count: u64,
    sum: f64,
    sum_squares: f64,
    min: f64,
    max: f64,
    vehicles: HashSet<u64>,
}

pub(crate) struct CarSpotSpeedSampler {
    output_dir: PathBuf,
    run: String,
    period_seconds: u32,
    iteration: u32,
    partition: u32,
    cells: BTreeMap<(String, u32), Cell>,
}

impl CarSpotSpeedSampler {
    pub(crate) fn new(
        output_dir: PathBuf,
        period_seconds: u32,
        iteration: u32,
        partition: u32,
    ) -> Self {
        let run = output_dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("run")
            .to_owned();
        Self {
            output_dir,
            run,
            period_seconds,
            iteration,
            partition,
            cells: BTreeMap::new(),
        }
    }

    pub(crate) fn period_seconds(&self) -> u32 {
        self.period_seconds
    }

    pub(crate) fn sample(&mut self, time_seconds: u64, samples: Vec<CarSpotSpeedSample>) {
        let hour = (time_seconds / 3600) as u32;
        if hour > 23 {
            return;
        }
        for sample in samples {
            let speed = sample.speed_mps * 3.6;
            if !speed.is_finite() || !(0.0..=MAX_SPEED_KMH).contains(&speed) {
                continue;
            }
            let cell = self.cells.entry((sample.link_id, hour)).or_default();
            if cell.count == 0 {
                cell.min = speed;
                cell.max = speed;
            } else {
                cell.min = cell.min.min(speed);
                cell.max = cell.max.max(speed);
            }
            cell.count += 1;
            cell.sum += speed;
            cell.sum_squares += speed * speed;
            cell.vehicles.insert(sample.vehicle_id);
        }
    }

    pub(crate) fn write(self) -> io::Result<PathBuf> {
        let dir = self
            .output_dir
            .join("ITERS")
            .join(format!("it.{}", self.iteration))
            .join("spot_speed");
        fs::create_dir_all(&dir)?;
        let path = dir.join(format!(
            "car_spot_speed_it{}_part{}.csv",
            self.iteration, self.partition
        ));
        let mut writer = csv::Writer::from_path(&path)?;
        writer.write_record([
            "run",
            "iteration",
            "interval",
            "time",
            "link_id",
            "speed_observations",
            "distinct_vehicles",
            "mean_speed_kmh",
            "sd_speed_kmh",
            "min_speed_kmh",
            "max_speed_kmh",
        ])?;
        for ((link_id, hour), cell) in self.cells {
            let mean = cell.sum / cell.count as f64;
            let sd = if cell.count < 2 {
                0.0
            } else {
                ((cell.sum_squares - cell.sum * cell.sum / cell.count as f64)
                    / (cell.count - 1) as f64)
                    .max(0.0)
                    .sqrt()
            };
            // Round down so serialized speeds cannot cross the link's sampled speed cap.
            let format_speed = |speed: f64| format!("{:.3}", (speed * 1_000.0).floor() / 1_000.0);
            writer.write_record([
                self.run.as_str(),
                &self.iteration.to_string(),
                &hour.to_string(),
                &format!("{hour:02}:00"),
                &link_id,
                &cell.count.to_string(),
                &cell.vehicles.len().to_string(),
                &format_speed(mean),
                &format!("{sd:.3}"),
                &format_speed(cell.min),
                &format_speed(cell.max),
            ])?;
        }
        writer.flush()?;
        Ok(path)
    }
}
