use crate::simulation::time::Tick;

/// Capacity which is still treated as no capacity. If the headway of a vehicle is a whole number
/// of ticks, the exact accumulated capacity reaches exactly 0 in some tick. Floating-point rounding
/// leaves a tiny positive or negative residue instead, and its sign depends on whether the capacity
/// was accumulated tick by tick or at once after a link was inactive. Without the margin, the
/// residue would decide whether a vehicle enters the buffer in that tick, so results would depend
/// on how often a link is updated, e.g. on the partitioning. With the margin, the vehicle enters in
/// the next tick, as with exact arithmetic. MATSim's DSim uses the same margin.
const ROUNDING_MARGIN: f64 = 1e-10;

/// Flow capacity of a link. As in MATSim's QSim (`QueueWithBuffer`), a vehicle consumes flow
/// capacity when it enters the buffer of the link, not when it leaves the buffer.
///
/// A vehicle may enter the buffer while `accumulated` exceeds [`ROUNDING_MARGIN`].
#[derive(Debug, Clone)]
pub struct Flowcap {
    last_update: Tick,
    // capacity available for the next vehicle to enter the buffer.
    accumulated: f64,
    capacity_per_tick: f64,
}

impl Flowcap {
    pub(super) fn new(capacity_per_tick: f64) -> Flowcap {
        Flowcap {
            last_update: Tick::zero(),
            accumulated: capacity_per_tick,
            capacity_per_tick,
        }
    }

    /// Accumulates capacity for the ticks elapsed since the last update. `pce_in_buffer` is the PCE
    /// of the vehicles which are currently in the buffer. They have already consumed capacity.
    ///
    /// The capacity is accumulated per tick, so that ticks need not be converted into times.
    pub(super) fn update_capacity(&mut self, now: Tick, pce_in_buffer: f64) {
        if self.last_update < now {
            let elapsed_ticks = (now - self.last_update).value() as f64;
            let acc_flow_cap = elapsed_ticks * self.capacity_per_tick + self.accumulated;
            self.accumulated = f64::min(acc_flow_cap, self.capacity_per_tick - pce_in_buffer);
            self.last_update = now;
        }
    }

    /// Tests whether another vehicle may enter the buffer.
    pub(super) fn has_capacity_left(&self) -> bool {
        self.accumulated > ROUNDING_MARGIN
    }

    /// Tests whether the capacity is at its maximum for an empty buffer. It stays there exactly,
    /// however many ticks pass until the next update.
    pub(super) fn is_full(&self) -> bool {
        self.accumulated >= self.capacity_per_tick
    }

    pub(super) fn consume(&mut self, by: f64) {
        self.accumulated -= by;
    }

    pub(super) fn capacity_per_tick(&self) -> f64 {
        self.capacity_per_tick
    }

    pub(super) fn accumulated(&self) -> f64 {
        self.accumulated
    }
}

#[cfg(test)]
mod tests {
    use assert_approx_eq::assert_approx_eq;

    use crate::simulation::network::flow_cap::Flowcap;
    use crate::simulation::time::{SimClock, Tick};
    use std::time::Duration;

    #[test]
    fn init() {
        let clock = SimClock::new(10);
        let cap = Flowcap::new(0.47401747 / 10.0);
        assert_approx_eq!(0.47401747 / 10.0, cap.capacity_per_tick(), 0.0001);
        assert_eq!(Duration::from_millis(100), clock.tick_length());
    }

    #[test]
    fn flowcap_consume_capacity() {
        let mut flowcap = Flowcap::new(1.0);
        assert!(flowcap.has_capacity_left());

        flowcap.consume(20.0);
        assert!(!flowcap.has_capacity_left());
    }

    #[test]
    fn flowcap_max_capacity_s() {
        let mut flowcap = Flowcap::new(1.0);

        flowcap.update_capacity(Tick::new(20), 0.0);

        assert_eq!(1.0, flowcap.accumulated);
    }

    #[test]
    fn flowcap_acc_capacity() {
        let mut flowcap = Flowcap::new(0.25);
        assert!(flowcap.has_capacity_left());

        flowcap.consume(1.0);
        assert!(!flowcap.has_capacity_left());

        flowcap.update_capacity(Tick::new(1), 0.0);
        assert!(!flowcap.has_capacity_left());

        // the accumulated capacity is exactly 0 and therefore not sufficient
        flowcap.update_capacity(Tick::new(3), 0.0);
        assert_eq!(0.0, flowcap.accumulated);
        assert!(!flowcap.has_capacity_left());

        flowcap.update_capacity(Tick::new(5), 0.0);
        assert!(flowcap.has_capacity_left());
    }

    #[test]
    fn flowcap_accumulation_is_capped_by_vehicles_in_buffer() {
        let mut flowcap = Flowcap::new(0.25);

        // a vehicle with pce 1 enters the buffer and stays there
        flowcap.consume(1.0);
        flowcap.update_capacity(Tick::new(100), 1.0);
        assert_eq!(-0.75, flowcap.accumulated);

        // once the vehicle has left the buffer, capacity accumulates again from that point
        flowcap.update_capacity(Tick::new(101), 0.0);
        assert_eq!(-0.5, flowcap.accumulated);
        flowcap.update_capacity(Tick::new(200), 0.0);
        assert_eq!(0.25, flowcap.accumulated);
    }

    #[test]
    fn flowcap_rounding_residue_is_no_capacity() {
        let mut flowcap = Flowcap::new(0.25);

        flowcap.consume(0.25 - 1e-14);
        assert!(flowcap.accumulated > 0.0);
        assert!(!flowcap.has_capacity_left());

        flowcap.update_capacity(Tick::new(1), 0.0);
        assert!(flowcap.has_capacity_left());
    }

    #[test]
    fn flowcap_full_capacity_stays_full() {
        let mut flowcap = Flowcap::new(0.25);

        flowcap.update_capacity(Tick::new(1), 0.0);
        assert_eq!(0.25, flowcap.accumulated);
        flowcap.update_capacity(Tick::new(1000), 0.0);
        assert_eq!(0.25, flowcap.accumulated);

        // the maximum excludes the pce of the vehicles in the buffer
        flowcap.update_capacity(Tick::new(1001), 1.0);
        assert_eq!(-0.75, flowcap.accumulated);
    }

    #[test]
    fn flowcap_update_is_idempotent_within_a_tick() {
        let mut flowcap = Flowcap::new(0.25);
        flowcap.consume(1.0);

        flowcap.update_capacity(Tick::new(1), 0.0);
        flowcap.update_capacity(Tick::new(1), 0.0);
        assert_eq!(-0.5, flowcap.accumulated);
    }
}
