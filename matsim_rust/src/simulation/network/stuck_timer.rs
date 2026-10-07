use crate::simulation::time::Tick;

#[derive(Debug, Clone)]
pub struct StuckTimer {
    timer_started: Option<Tick>,
    stuck_threshold: Tick,
}

impl StuckTimer {
    pub fn new(stuck_threshold: Tick) -> Self {
        StuckTimer {
            timer_started: None,
            stuck_threshold,
        }
    }

    pub fn restart(&mut self, now: impl Into<Tick>) {
        self.timer_started = Some(now.into());
    }

    pub fn is_stuck(&self, now: impl Into<Tick>) -> bool {
        let now = now.into();
        if let Some(time) = self.timer_started {
            now - time >= self.stuck_threshold
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::simulation::network::stuck_timer::StuckTimer;
    use crate::simulation::time::Tick;

    #[test]
    fn init() {
        let timer = StuckTimer::new(Tick::new(42));
        assert!(timer.timer_started.is_none());
        assert_eq!(Tick::new(42), timer.stuck_threshold);
    }

    #[test]
    fn restart() {
        let mut timer = StuckTimer::new(Tick::new(42));

        timer.restart(Tick::new(1));
        timer.restart(Tick::new(2));

        assert!(timer.timer_started.is_some());
        assert_eq!(Tick::new(2), timer.timer_started.unwrap());
    }

    #[test]
    fn is_stuck() {
        let mut timer = StuckTimer::new(Tick::new(42));

        timer.restart(Tick::new(17));
        assert!(!timer.is_stuck(Tick::new(18)));
        assert!(timer.is_stuck(Tick::new(17 + 42)));

        timer.restart(Tick::new(18));
        assert!(!timer.is_stuck(Tick::new(17 + 42)));
    }
}
