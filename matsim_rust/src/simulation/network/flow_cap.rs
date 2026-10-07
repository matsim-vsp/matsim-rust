use crate::simulation::time::SimTime;

#[derive(Debug, Clone)]
pub struct Flowcap {
    last_update_time: SimTime,
    remaining_capacity: f64,
    capacity_per_second: f64,
    capacity_per_tick: f64,
}

impl Flowcap {
    pub(super) fn new(capacity_h: f64, sample_size: f64, capacity_per_tick: f64) -> Flowcap {
        let capacity_per_second = capacity_h * sample_size / 3600.;
        Flowcap {
            last_update_time: SimTime::default(),
            remaining_capacity: capacity_per_tick,
            capacity_per_second,
            capacity_per_tick,
        }
    }

    pub(super) fn update_capacity(&mut self, now: SimTime) {
        if self.last_update_time < now {
            let elapsed = now.duration_since(self.last_update_time).as_secs_f64();
            let acc_flow_cap = elapsed * self.capacity_per_second + self.remaining_capacity;
            self.remaining_capacity = f64::min(acc_flow_cap, self.capacity_per_tick);
            self.last_update_time = now;
        }
    }

    pub(super) fn has_capacity_left(&self) -> bool {
        self.remaining_capacity > 1e-10
    }

    pub(super) fn remaining_capacity(&self) -> f64 {
        self.remaining_capacity
    }

    pub(super) fn consume(&mut self, by: f64) {
        self.remaining_capacity -= by;
    }

    pub(super) fn capacity_per_tick(&self) -> f64 {
        self.capacity_per_tick
    }
}

#[cfg(test)]
mod tests {
    use assert_approx_eq::assert_approx_eq;

    use crate::simulation::network::flow_cap::Flowcap;
    use crate::simulation::time::{SimClock, SimTime};
    use std::time::Duration;

    #[test]
    fn init() {
        let clock = SimClock::new(10);
        let cap = Flowcap::new(5432., 0.31415, 0.47401747 / 10.0);
        assert_approx_eq!(0.47401747 / 10.0, cap.capacity_per_tick(), 0.0001);
        assert_eq!(Duration::from_millis(100), clock.tick_length());
    }

    #[test]
    fn flowcap_consume_capacity() {
        let mut flowcap = Flowcap::new(36000., 1., 1.0);
        assert!(flowcap.has_capacity_left());

        flowcap.consume(20.0);
        assert!(!flowcap.has_capacity_left());
    }

    #[test]
    fn flowcap_max_capacity_s() {
        let mut flowcap = Flowcap::new(36000., 1., 1.0);

        flowcap.update_capacity(SimTime::from_secs(20));

        assert_eq!(1.0, flowcap.remaining_capacity);
    }

    #[test]
    fn flowcap_acc_capacity() {
        let mut flowcap = Flowcap::new(900., 1., 0.25);
        assert!(flowcap.has_capacity_left());

        flowcap.consume(1.0);
        assert!(!flowcap.has_capacity_left());

        flowcap.update_capacity(SimTime::from_secs(1));
        assert!(!flowcap.has_capacity_left());

        flowcap.update_capacity(SimTime::from_secs(3));
        assert!(!flowcap.has_capacity_left());

        flowcap.update_capacity(SimTime::from_secs(5));
        assert!(flowcap.has_capacity_left());
    }
}
