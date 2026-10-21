//! Benchmark for the barrier that synchronizes the partitions in every time step if
//! `computational_setup.global_sync` is enabled. It compares `std::sync::Barrier` with the
//! spin-then-park barrier in `simulation::messaging::sim_communication::barrier`. Each thread only
//! waits at the barrier, optionally with some busy work in between, so the time per wait is the
//! lower bound that the barrier puts on a simulation step.
//!
//! Run the benchmark in release mode:
//!
//! ```shell
//! cargo test -p matsim-rust --release --test barrier_benchmark -- --ignored --nocapture --test-threads=1
//! ```
//!
//! The thread counts can be set as a comma-separated list with `MATSIM_RUST_BARRIER_BENCH_THREADS`.
//! By default, the benchmark runs 1, 2, 4, 8, 32 and as many threads as there are cores. The number
//! of measured waits per thread can be set with `MATSIM_RUST_BARRIER_BENCH_ITERATIONS`.
//!
//! `MATSIM_RUST_BARRIER_BENCH_WORK_NS` adds a busy work of a random duration between 0 and the
//! given number of nanoseconds before each wait, which simulates partitions that need different
//! times for a step. The durations are drawn from a fixed seed per thread, so both barriers get the
//! same work. The measured time per wait includes the work, i.e. roughly the longest work of all
//! threads.
//!
//! The threads are not pinned to cores. Pin the test binary with `taskset` or `numactl` if needed.

use macros::deterministic_id_test;
use matsim_rust::simulation::messaging::sim_communication::barrier::{Barrier, WaitPolicy};
use matsim_rust::simulation::random::get_rng;
use rand::RngExt;
use std::time::{Duration, Instant};

const THREADS_ENV: &str = "MATSIM_RUST_BARRIER_BENCH_THREADS";
const ITERATIONS_ENV: &str = "MATSIM_RUST_BARRIER_BENCH_ITERATIONS";
const WORK_NS_ENV: &str = "MATSIM_RUST_BARRIER_BENCH_WORK_NS";
const DEFAULT_ITERATIONS: u64 = 10_000;
const WARMUP_WAITS: u64 = 1_000;
const REPETITIONS: usize = 5;
const SEED: u64 = 42;

/// A barrier implementation under test.
trait BenchBarrier: Sync {
    const NAME: &'static str;

    fn new(threads: usize) -> Self;

    fn wait(&self);
}

impl BenchBarrier for std::sync::Barrier {
    const NAME: &'static str = "std::sync::Barrier";

    fn new(threads: usize) -> Self {
        std::sync::Barrier::new(threads)
    }

    fn wait(&self) {
        std::sync::Barrier::wait(self);
    }
}

impl BenchBarrier for Barrier {
    const NAME: &'static str = "spin-then-park";

    fn new(threads: usize) -> Self {
        Barrier::with_policy(threads, WaitPolicy::default())
    }

    fn wait(&self) {
        Barrier::wait(self);
    }
}

#[deterministic_id_test(matsim_rust)]
#[ignore = "benchmark"]
fn wait_at_barrier() {
    let cores = std::thread::available_parallelism().unwrap().get();
    let threads: Vec<usize> = match std::env::var(THREADS_ENV) {
        Ok(threads) => threads
            .split(',')
            .map(|t| t.trim().parse().expect("Invalid thread count"))
            .collect(),
        Err(_) => {
            let mut threads = vec![1, 2, 4, 8, 32, cores];
            threads.sort_unstable();
            threads.dedup();
            threads
        }
    };
    let iterations = std::env::var(ITERATIONS_ENV)
        .map(|i| i.parse().expect("Invalid number of iterations"))
        .unwrap_or(DEFAULT_ITERATIONS);
    let work_ns: u64 = std::env::var(WORK_NS_ENV)
        .map(|w| w.parse().expect("Invalid work"))
        .unwrap_or(0);

    println!(
        "cores: {cores}, iterations: {iterations}, warmup: {WARMUP_WAITS}, repetitions: {REPETITIONS}"
    );
    for &t in threads.iter().filter(|&&t| t > cores) {
        println!(
            "warning: {t} threads on {cores} cores, spinning threads keep others from running"
        );
    }
    println!();
    println!("| implementation | threads | work (ns) | ns/wait (min) | ns/wait (median) |");
    println!("|---|---:|---:|---:|---:|");
    for t in threads {
        print_row::<std::sync::Barrier>(t, iterations, work_ns);
        print_row::<Barrier>(t, iterations, work_ns);
    }
}

fn print_row<B: BenchBarrier>(threads: usize, iterations: u64, work_ns: u64) {
    let mut durations: Vec<Duration> = (0..REPETITIONS)
        .map(|_| run::<B>(threads, iterations, work_ns))
        .collect();
    durations.sort_unstable();
    let ns_per_wait = |d: Duration| d.as_nanos() as f64 / iterations as f64;
    println!(
        "| {} | {threads} | {work_ns} | {:.0} | {:.0} |",
        B::NAME,
        ns_per_wait(durations[0]),
        ns_per_wait(durations[REPETITIONS / 2])
    );
}

/// Lets `threads` threads wait `WARMUP_WAITS` and then `iterations` times at a barrier and returns
/// the duration of the measured waits, as seen by thread 0.
fn run<B: BenchBarrier>(threads: usize, iterations: u64, work_ns: u64) -> Duration {
    let barrier = B::new(threads);

    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..threads)
            .map(|thread| {
                let barrier = &barrier;
                scope.spawn(move || {
                    let mut rng = get_rng(SEED, "barrier-benchmark-work", &thread.to_string());
                    let mut work = move || {
                        if work_ns > 0 {
                            busy_wait(Duration::from_nanos(rng.random_range(0..work_ns)));
                        }
                    };

                    for _ in 0..WARMUP_WAITS {
                        work();
                        barrier.wait();
                    }
                    let start = Instant::now();
                    for _ in 0..iterations {
                        work();
                        barrier.wait();
                    }
                    (thread == 0).then(|| start.elapsed())
                })
            })
            .collect();

        handles
            .into_iter()
            .filter_map(|handle| handle.join().unwrap())
            .next()
            .unwrap()
    })
}

fn busy_wait(duration: Duration) {
    let start = Instant::now();
    while start.elapsed() < duration {
        std::hint::spin_loop();
    }
}
