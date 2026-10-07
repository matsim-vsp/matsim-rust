use matsim_rust::simulation::config::Config;

fn main() {
    let mut config = Config::default();
    config.qsim_mut().stuck_threshold = 42;
    print!("{:?}", config);
}
