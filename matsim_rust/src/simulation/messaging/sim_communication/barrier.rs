//! A barrier for a fixed number of threads that first spins, then yields and finally sleeps.
//!
//! [`Barrier`] is a drop-in replacement for [`std::sync::Barrier`]. Each arriving thread
//! increments a shared counter; the last one resets it and opens the barrier by incrementing a
//! generation counter. All other threads wait until the generation changes: they busy-wait for a
//! short time, then yield their core between checks, and finally sleep on the generation counter
//! until the last thread wakes them up with a single system call.
//!
//! # Guarantees
//!
//! - Everything that any thread did before its call to [`Barrier::wait`] happens before
//!   everything that any thread does after its call to `wait` of the same generation returns.
//! - Exactly one thread per generation is the leader, namely the one that arrived last.
//!
//! # Contract
//!
//! The barrier is meant for exactly `n` participating threads, and each of them calls
//! [`Barrier::wait`] exactly once per generation. More threads or more calls break the
//! barrier. If a participant panics before it calls `wait`, the others wait forever, as with
//! [`std::sync::Barrier`].
//!
//! The module only depends on `std` and `atomic-wait`, which provides the futex-like
//! `wait`/`wake_all` that `std` does not expose.

use std::fmt;
use std::ops::Deref;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// How long a thread waiting at a [`Barrier`] busy-waits and yields before it sleeps. Both
/// durations are measured from the moment the thread starts waiting.
///
/// Setting both to zero makes waiting threads sleep immediately.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WaitPolicy {
    /// How long a waiting thread busy-waits.
    pub spin: Duration,
    /// Until when a waiting thread yields its core between checks, once it no longer busy-waits.
    pub yield_until: Duration,
}

impl Default for WaitPolicy {
    fn default() -> Self {
        Self {
            spin: Duration::from_micros(5),
            yield_until: Duration::from_micros(50),
        }
    }
}

/// Aligns a value to its own cache line, so that writing it does not invalidate the cache line of
/// other values. 128 bytes covers CPUs that fetch pairs of 64-byte lines (e.g. Intel's spatial
/// prefetcher) and CPUs with 128-byte lines (e.g. Apple's M series).
#[repr(align(128))]
struct CachePadded<T>(T);

impl<T> Deref for CachePadded<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.0
    }
}

/// A barrier that synchronizes a fixed number of threads, see the [module documentation](self).
///
/// Share it between threads, e.g. with an `Arc`.
pub struct Barrier {
    /// The number of threads that arrived in the current generation.
    arrived: CachePadded<AtomicU32>,
    /// Incremented by the last thread of a generation to open the barrier. It wraps around after
    /// `u32::MAX` and is the word that sleeping threads wait on.
    generation: CachePadded<AtomicU32>,
    /// The number of threads that sleep or are about to sleep.
    sleepers: CachePadded<AtomicU32>,
    /// The number of participating threads, at least 1. Never written after construction, so it
    /// shares a cache line with `policy` without slowing down the counters.
    n: u32,
    policy: WaitPolicy,
}

/// Returned by [`Barrier::wait`] to tell exactly one thread per generation that it is the leader.
#[derive(Debug)]
pub struct BarrierWaitResult {
    is_leader: bool,
}

impl BarrierWaitResult {
    /// Returns whether this thread arrived last at the barrier. Exactly one thread per generation
    /// is the leader.
    pub fn is_leader(&self) -> bool {
        self.is_leader
    }
}

impl Barrier {
    /// Creates a barrier for `n` threads with the default [`WaitPolicy`].
    ///
    /// As with [`std::sync::Barrier`], a barrier for 0 threads behaves like a barrier for 1
    /// thread, i.e. `wait` returns immediately.
    ///
    /// # Panics
    ///
    /// Panics if `n` is larger than `u32::MAX`.
    pub fn new(n: usize) -> Self {
        Self::with_policy(n, WaitPolicy::default())
    }

    /// Creates a barrier for `n` threads that wait as specified by `policy`.
    ///
    /// # Panics
    ///
    /// Panics if `n` is larger than `u32::MAX`.
    pub fn with_policy(n: usize, policy: WaitPolicy) -> Self {
        Self::create(n, policy, 0)
    }

    /// Creates a barrier that starts at `generation` instead of 0, to test the wrap-around.
    #[cfg(test)]
    fn with_generation(n: usize, policy: WaitPolicy, generation: u32) -> Self {
        Self::create(n, policy, generation)
    }

    fn create(n: usize, policy: WaitPolicy, generation: u32) -> Self {
        let n = u32::try_from(n).unwrap_or_else(|_| {
            panic!(
                "A barrier supports at most {} threads, but {n} were requested",
                u32::MAX
            )
        });
        Self {
            arrived: CachePadded(AtomicU32::new(0)),
            generation: CachePadded(AtomicU32::new(generation)),
            sleepers: CachePadded(AtomicU32::new(0)),
            n: n.max(1),
            policy,
        }
    }

    /// Blocks until all `n` threads have called `wait`. Exactly one of them, the last to arrive,
    /// receives a result whose [`BarrierWaitResult::is_leader`] returns `true`.
    pub fn wait(&self) -> BarrierWaitResult {
        // Read the generation before arriving. The barrier cannot open before this thread has
        // arrived, so this is the generation that this thread waits for to end.
        let generation = self.generation.load(Ordering::Acquire);

        let prev = self.arrived.fetch_add(1, Ordering::AcqRel);
        if prev + 1 == self.n {
            // This is the last caller of `wait` in this generation, so it opens the barrier for the others and immediately returns.

            self.arrived.store(0, Ordering::Relaxed);
            self.generation.fetch_add(1, Ordering::SeqCst);

            // Together with `sleep`, this is Dekker's pattern: this thread writes `generation`
            // and then reads `sleepers`, a sleeping thread increments `sleepers` and then reads
            // `generation`, all with sequentially consistent ordering. Hence, either this thread
            // sees the sleeper, or the sleeper sees the new generation and does not sleep. A
            // count that is too large because a sleeper of an earlier generation has not yet
            // decremented it only costs an unnecessary wake-up call.
            if self.sleepers.load(Ordering::SeqCst) > 0 {
                atomic_wait::wake_all(&*self.generation);
            }
            return BarrierWaitResult { is_leader: true };
        }

        self.wait_for_next(generation);
        BarrierWaitResult { is_leader: false }
    }

    /// Waits until the generation differs from `generation`. Busy-waits and yields as specified by
    /// the policy, then sleeps.
    fn wait_for_next(&self, generation: u32) {
        // The acquire loads synchronize with the opening of the barrier, see `wait`.
        if self.generation.load(Ordering::Acquire) != generation {
            return;
        }

        let start = Instant::now();
        loop {
            let waited = start.elapsed();
            if waited < self.policy.spin {
                std::hint::spin_loop();
            } else if waited < self.policy.yield_until {
                std::thread::yield_now();
            } else {
                break;
            }
            if self.generation.load(Ordering::Acquire) != generation {
                return;
            }
        }

        self.sleep(generation);
    }

    /// Sleeps until the generation differs from `generation`.
    fn sleep(&self, generation: u32) {
        // Announce the sleeper before checking the generation, see the leader in `wait`.
        self.sleepers.fetch_add(1, Ordering::SeqCst);
        // `atomic_wait::wait` only sleeps if the generation still equals `generation`, which the
        // operating system checks atomically with going to sleep (a futex on Linux). Thus, a
        // wake-up between the check of the loop condition and the call cannot be lost. The loop
        // catches spurious wake-ups.
        while self.generation.load(Ordering::SeqCst) == generation {
            atomic_wait::wait(&self.generation, generation);
        }
        self.sleepers.fetch_sub(1, Ordering::Relaxed);
    }
}

impl fmt::Debug for Barrier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Barrier")
            .field("n", &self.n)
            .field("policy", &self.policy)
            .field("arrived", &self.arrived.load(Ordering::Relaxed))
            .field("generation", &self.generation.load(Ordering::Relaxed))
            .field("sleepers", &self.sleepers.load(Ordering::Relaxed))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::{Barrier, WaitPolicy};
    use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
    use std::sync::mpsc::{self, RecvTimeoutError};
    use std::thread;
    use std::time::Duration;

    /// How long a test may take before it is considered to hang.
    const TIMEOUT: Duration = Duration::from_secs(60);

    const SLEEP_IMMEDIATELY: WaitPolicy = WaitPolicy {
        spin: Duration::ZERO,
        yield_until: Duration::ZERO,
    };

    fn cores() -> usize {
        thread::available_parallelism().unwrap().get()
    }

    /// Runs `scenario` on its own thread and fails if it does not finish within `TIMEOUT`, so
    /// that a barrier that hangs fails the test instead of blocking it forever. A panic of the
    /// scenario is propagated.
    fn with_watchdog<T: Send + 'static>(scenario: impl FnOnce() -> T + Send + 'static) -> T {
        let (sender, receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            let result = scenario();
            // The receiver is gone if the test has already failed with a timeout.
            let _ = sender.send(());
            result
        });
        match receiver.recv_timeout(TIMEOUT) {
            // The sender has been dropped without sending if the scenario panicked.
            Ok(()) | Err(RecvTimeoutError::Disconnected) => handle
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
            Err(RecvTimeoutError::Timeout) => {
                panic!("The scenario did not finish within {TIMEOUT:?}, the barrier hangs")
            }
        }
    }

    /// What the threads observed in `run`.
    struct Outcome {
        /// How often a thread returned from `wait` before all threads had arrived.
        early_returns: u64,
        /// The number of leaders of each generation.
        leaders: Vec<u32>,
        /// The generation of the barrier afterwards.
        generation: u32,
    }

    impl Outcome {
        fn assert_correct(&self) {
            assert_eq!(
                0, self.early_returns,
                "Threads returned from wait too early"
            );
            for (generation, &leaders) in self.leaders.iter().enumerate() {
                assert_eq!(1, leaders, "Generation {generation} had {leaders} leaders");
            }
        }
    }

    /// Lets `barrier.n` threads pass `generations` generations of `barrier`. Before `wait`, each
    /// thread increments the counter of the generation; after `wait`, the counter must equal the
    /// number of threads. The threads record violations instead of panicking, so that the others
    /// do not wait forever for a thread that panicked.
    fn run(barrier: Barrier, generations: usize) -> Outcome {
        with_watchdog(move || {
            let threads = barrier.n;
            let arrivals: Vec<AtomicU32> = (0..generations).map(|_| AtomicU32::new(0)).collect();
            let leaders: Vec<AtomicU32> = (0..generations).map(|_| AtomicU32::new(0)).collect();
            let early_returns = AtomicU64::new(0);

            thread::scope(|scope| {
                for _ in 0..threads {
                    scope.spawn(|| {
                        for generation in 0..generations {
                            // Relaxed, so that only the barrier makes the increments visible.
                            arrivals[generation].fetch_add(1, Ordering::Relaxed);
                            if barrier.wait().is_leader() {
                                leaders[generation].fetch_add(1, Ordering::Relaxed);
                            }
                            if arrivals[generation].load(Ordering::Relaxed) != threads {
                                early_returns.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    });
                }
            });

            assert_eq!(0, barrier.arrived.load(Ordering::Relaxed));
            assert_eq!(0, barrier.sleepers.load(Ordering::Relaxed));
            Outcome {
                early_returns: early_returns.into_inner(),
                leaders: leaders.into_iter().map(AtomicU32::into_inner).collect(),
                generation: barrier.generation.load(Ordering::Relaxed),
            }
        })
    }

    /// The thread counts and policies of the tests for the basic guarantees.
    fn configurations() -> Vec<(usize, WaitPolicy)> {
        let mut threads = vec![2, 3, cores()];
        threads.sort_unstable();
        threads.dedup();
        threads
            .into_iter()
            .flat_map(|t| [(t, WaitPolicy::default()), (t, SLEEP_IMMEDIATELY)])
            .collect()
    }

    #[test]
    fn barrier_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Barrier>();
    }

    #[test]
    fn single_thread_never_blocks() {
        for n in [0, 1] {
            for policy in [WaitPolicy::default(), SLEEP_IMMEDIATELY] {
                let barrier = Barrier::with_policy(n, policy);
                for _ in 0..10_000 {
                    assert!(barrier.wait().is_leader());
                }
                assert_eq!(10_000, barrier.generation.load(Ordering::Relaxed));
                assert_eq!(0, barrier.arrived.load(Ordering::Relaxed));
            }
        }
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    #[should_panic(expected = "at most 4294967295 threads")]
    fn more_than_u32_max_threads_panics() {
        Barrier::new(u32::MAX as usize + 1);
    }

    #[test]
    fn no_thread_returns_early() {
        for (threads, policy) in configurations() {
            let outcome = run(Barrier::with_policy(threads, policy), 1_000);
            assert_eq!(
                0, outcome.early_returns,
                "{threads} threads with {policy:?} returned from wait too early"
            );
        }
    }

    #[test]
    fn exactly_one_leader_per_generation() {
        for (threads, policy) in configurations() {
            let outcome = run(Barrier::with_policy(threads, policy), 1_000);
            for (generation, &leaders) in outcome.leaders.iter().enumerate() {
                assert_eq!(
                    1, leaders,
                    "{threads} threads with {policy:?} had {leaders} leaders in generation {generation}"
                );
            }
        }
    }

    /// Each thread writes its slot before `wait` and reads all slots after `wait`, all relaxed.
    /// The slots alternate between two buffers, because a thread writes the next generation's
    /// value while others may still read the current one.
    #[test]
    fn writes_before_wait_are_visible_after_wait() {
        fn value(thread: usize, generation: usize) -> u64 {
            ((generation as u64) << 32) | thread as u64
        }

        for (threads, policy) in configurations() {
            let mismatches = with_watchdog(move || {
                let generations = 1_000;
                let barrier = Barrier::with_policy(threads, policy);
                let slots: [Vec<AtomicU64>; 2] =
                    std::array::from_fn(|_| (0..threads).map(|_| AtomicU64::new(0)).collect());
                let mismatches = AtomicU64::new(0);

                thread::scope(|scope| {
                    for thread in 0..threads {
                        let (barrier, slots, mismatches) = (&barrier, &slots, &mismatches);
                        scope.spawn(move || {
                            for generation in 0..generations {
                                let slots = &slots[generation % 2];
                                slots[thread].store(value(thread, generation), Ordering::Relaxed);
                                barrier.wait();
                                for (other, slot) in slots.iter().enumerate() {
                                    if slot.load(Ordering::Relaxed) != value(other, generation) {
                                        mismatches.fetch_add(1, Ordering::Relaxed);
                                    }
                                }
                            }
                        });
                    }
                });
                mismatches.into_inner()
            });
            assert_eq!(
                0, mismatches,
                "{threads} threads with {policy:?} missed writes of other threads"
            );
        }
    }

    #[test]
    fn more_threads_than_cores_do_not_hang() {
        for policy in [WaitPolicy::default(), SLEEP_IMMEDIATELY] {
            run(Barrier::with_policy(2 * cores(), policy), 100).assert_correct();
        }
    }

    /// One thread arrives late in every generation, so the others have to sleep. Before it
    /// arrives, it waits until all others are counted as sleepers, so that the test does not
    /// depend on how long the others need to fall asleep.
    #[test]
    fn sleeping_threads_are_woken_up() {
        let threads = 4;
        let generations = 5;
        let barrier = with_watchdog(move || {
            let barrier = Barrier::with_policy(threads, SLEEP_IMMEDIATELY);
            thread::scope(|scope| {
                for thread in 0..threads {
                    let barrier = &barrier;
                    scope.spawn(move || {
                        for _ in 0..generations {
                            if thread == 0 {
                                thread::sleep(Duration::from_millis(2));
                                // Once all others have arrived, they have left the previous
                                // generation and no longer count as its sleepers.
                                while barrier.arrived.load(Ordering::SeqCst) < threads as u32 - 1
                                    || barrier.sleepers.load(Ordering::SeqCst) < threads as u32 - 1
                                {
                                    thread::yield_now();
                                }
                            }
                            barrier.wait();
                        }
                    });
                }
            });
            barrier
        });
        assert_eq!(generations, barrier.generation.load(Ordering::Relaxed));
        assert_eq!(0, barrier.sleepers.load(Ordering::Relaxed));
        assert_eq!(0, barrier.arrived.load(Ordering::Relaxed));
    }

    #[test]
    fn generation_wraps_around() {
        for policy in [WaitPolicy::default(), SLEEP_IMMEDIATELY] {
            let barrier = Barrier::with_generation(3, policy, u32::MAX - 2);
            let outcome = run(barrier, 10);
            outcome.assert_correct();
            assert_eq!(7, outcome.generation);
        }
    }

    #[test]
    fn debug_shows_the_state() {
        let barrier = Barrier::new(3);
        let debug = format!("{barrier:?}");
        assert!(
            debug.starts_with("Barrier { n: 3, policy: WaitPolicy"),
            "{debug}"
        );
        assert!(debug.contains("generation: 0"), "{debug}");
        assert_eq!(
            "BarrierWaitResult { is_leader: true }",
            format!("{:?}", Barrier::new(1).wait())
        );
    }
}
