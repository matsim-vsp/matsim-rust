use clap::Parser;
use rust_qsim::simulation::id;
use rust_qsim::simulation::scenario::population::Population;
use rust_qsim::simulation::scenario::vehicles::Garage;
use std::path::PathBuf;

fn main() {
    rust_qsim::simulation::logging::init_std_out_logging_thread_local();
    let args = InputArgs::parse();

    id::load_from_file(&args.id_path);

    let mut garage = Garage::from_file(&args.garage_path);
    let mut population = Population::from_file(&args.pop_path, &mut garage);

    // take the first num_pop persons
    let mut count: i32 = args.num_pop as i32;
    population.persons.retain(|_id, _person| {
        if count > 0 {
            count -= 1;
            true
        } else {
            false
        }
    });

    let current_extension = args
        .pop_path
        .extension()
        .expect("Population path should have extension.");
    let out_path = args
        .pop_path
        .with_extension(args.num_pop.to_string())
        .with_added_extension(current_extension);

    population.to_file(&out_path);
}

#[derive(Parser, Debug)]
struct InputArgs {
    #[arg(short, long)]
    pub pop_path: PathBuf,
    #[arg(short, long)]
    pub garage_path: PathBuf,
    #[arg(short, long)]
    pub id_path: PathBuf,
    #[arg(short, long)]
    pub num_pop: u32,
}
