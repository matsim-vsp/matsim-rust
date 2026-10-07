use clap::Parser;
use matsim_rust::simulation::id;
use matsim_rust::simulation::scenario::population::Population;
use matsim_rust::simulation::scenario::vehicles::Garage;
use std::path::PathBuf;

fn main() {
    matsim_rust::simulation::logging::init_std_out_logging_thread_local();
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

    let folder = args.pop_path.parent().unwrap();
    let mut name_parts: Vec<&str> = args
        .pop_path
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .split('.')
        .collect();
    let num_parts_string = args.num_pop.to_string();
    name_parts.insert(name_parts.len() - 1, num_parts_string.as_str());
    let out_path = folder.join(name_parts.join("."));

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
