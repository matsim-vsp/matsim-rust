use matsim_rust::simulation::config::{Config, TrafficDynamics};

fn main() {
    let mut config = Config::default();
    config.qsim_mut().stuck_threshold = 42;
    // Queue is the default. Opt in to backward-moving gaps and a separate inflow budget.
    config.qsim_mut().traffic_dynamics = TrafficDynamics::KinematicWaves;
    print!("{:?}", config);
}
