//! Door operation of a transit vehicle at a stop. Port of MATSim's `ComplexTransitStopHandler`,
//! which QSim uses whenever transit runs in the mobsim.
//!
//! The handler is asked once per dwell second. Each call opens or closes the doors, or lets
//! passengers through them, and returns how long the vehicle has to stay; `0` means it may leave.
//! Per-person access and egress times accumulate as fractions of a second, so a time below one
//! second lets several people through per call and a longer one makes later calls wait.

use crate::simulation::scenario::vehicles::InternalVehicleType;

const OPEN_DOORS_DURATION: f64 = 1.0;
const CLOSE_DOORS_DURATION: f64 = 1.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum DoorOperation {
    /// Everyone alights before anyone boards.
    Serial,
    /// Boarding and alighting happen at the same time.
    Parallel,
}

/// What the vehicle does during one call: how many of the leaving and entering passengers pass
/// the doors, and how long it stays before the next call.
#[derive(Debug, PartialEq)]
pub(crate) struct DoorStep {
    pub alight: usize,
    pub board: usize,
    pub stop_time: f64,
}

#[derive(Debug, Clone)]
pub(crate) struct Doors {
    operation: DoorOperation,
    access_time: f64,
    egress_time: f64,
    open: bool,
    leaving_fraction: f64,
    entering_fraction: f64,
}

impl Doors {
    pub(crate) fn new(operation: DoorOperation, access_time: f64, egress_time: f64) -> Self {
        Self {
            operation,
            access_time,
            egress_time,
            open: false,
            leaving_fraction: 0.0,
            entering_fraction: 0.0,
        }
    }

    /// MATSim's `VehicleUtils` defaults: serial doors and one second per person each way.
    pub(crate) fn for_vehicle_type(vehicle_type: &InternalVehicleType) -> Self {
        let attributes = &vehicle_type.attributes;
        let operation = match attributes.get::<String>("doorOperationMode").as_deref() {
            None => DoorOperation::Serial,
            Some(mode) if mode.eq_ignore_ascii_case("serial") => DoorOperation::Serial,
            Some(mode) if mode.eq_ignore_ascii_case("parallel") => DoorOperation::Parallel,
            Some(mode) => panic!(
                "Vehicle type {} has unsupported door operation mode {mode}.",
                vehicle_type.id
            ),
        };
        let seconds = |key: &str| attributes.get::<f64>(key).unwrap_or(1.0);
        Self::new(
            operation,
            seconds("accessTimeInSecondsPerPerson"),
            seconds("egressTimeInSecondsPerPerson"),
        )
    }

    /// One call of the handler with the passengers who currently want to leave and enter.
    pub(crate) fn step(&mut self, leaving: usize, entering: usize) -> DoorStep {
        let mut step = DoorStep {
            alight: 0,
            board: 0,
            stop_time: 0.0,
        };
        if !self.open {
            if leaving > 0 || entering > 0 {
                self.open = true;
                step.stop_time = OPEN_DOORS_DURATION;
            }
            return step;
        }

        if leaving == 0 && entering == 0 {
            if self.entering_fraction < 1.0 && self.leaving_fraction < 1.0 {
                self.open = false;
                self.entering_fraction = 0.0;
                self.leaving_fraction = 0.0;
                step.stop_time = CLOSE_DOORS_DURATION;
            }
            // Someone is still passing the doors, so wait again.
            if self.entering_fraction >= 1.0 {
                self.entering_fraction -= 1.0;
                step.stop_time = 1.0;
            }
            if self.leaving_fraction >= 1.0 {
                self.leaving_fraction -= 1.0;
                step.stop_time = 1.0;
            }
            return step;
        }

        match self.operation {
            DoorOperation::Serial => {
                if leaving > 0 {
                    step.alight = pass(&mut self.leaving_fraction, self.egress_time, leaving);
                    step.stop_time = 1.0;
                } else {
                    settle(&mut self.leaving_fraction);
                    if entering > 0 {
                        step.board = pass(&mut self.entering_fraction, self.access_time, entering);
                        step.stop_time = 1.0;
                    } else {
                        settle(&mut self.entering_fraction);
                    }
                }
            }
            DoorOperation::Parallel => {
                if entering > 0 {
                    step.board = pass(&mut self.entering_fraction, self.access_time, entering);
                    step.stop_time = 1.0;
                } else {
                    settle(&mut self.entering_fraction);
                }
                if leaving > 0 {
                    step.alight = pass(&mut self.leaving_fraction, self.egress_time, leaving);
                    step.stop_time = 1.0;
                } else {
                    settle(&mut self.leaving_fraction);
                }
            }
        }
        step
    }
}

/// Lets passengers through one door for one second; returns how many passed.
fn pass(fraction: &mut f64, seconds_per_person: f64, waiting: usize) -> usize {
    let mut passed = 0;
    // A fraction of one or more means the previous person still occupies the door.
    while *fraction < 1.0 && passed < waiting {
        passed += 1;
        *fraction += seconds_per_person;
    }
    *fraction -= 1.0;
    passed
}

/// A door nobody uses this second frees up by one second, but never below zero.
fn settle(fraction: &mut f64) {
    *fraction = (*fraction - 1.0).max(0.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs the handler the way the stop logic does: once per second until it returns 0, with
    /// the passenger counts shrinking as people pass.
    fn dwell(doors: &mut Doors, mut leaving: usize, mut entering: usize) -> Vec<DoorStep> {
        let mut steps = Vec::new();
        loop {
            let step = doors.step(leaving, entering);
            leaving -= step.alight;
            entering -= step.board;
            let done = step.stop_time == 0.0;
            steps.push(step);
            if done {
                return steps;
            }
        }
    }

    #[test]
    fn serial_boarding_follows_complex_transit_stop_handler() {
        // Walked off `ComplexTransitStopHandler.handleSerialStop` with MATSim's default one
        // second per person: one second to open, one person per second, then one to close.
        // That this reproduces a real Java run is checked by the event comparison against a
        // Java run of the pt_tutorial scenario, which compares boarding and alighting times
        // per vehicle and per second.
        let mut doors = Doors::new(DoorOperation::Serial, 1.0, 1.0);
        let steps = dwell(&mut doors, 0, 6);
        let boards: Vec<_> = steps.iter().map(|s| s.board).collect();
        assert_eq!(vec![0, 1, 1, 1, 1, 1, 1, 0, 0], boards);
        let dwell_seconds: f64 = steps.iter().map(|s| s.stop_time).sum();
        assert_eq!(8.0, dwell_seconds);
    }

    #[test]
    fn serial_doors_let_everyone_off_before_anyone_boards() {
        let mut doors = Doors::new(DoorOperation::Serial, 1.0, 1.0);
        let steps = dwell(&mut doors, 2, 2);
        let order: Vec<_> = steps.iter().map(|s| (s.alight, s.board)).collect();
        assert_eq!(
            vec![(0, 0), (1, 0), (1, 0), (0, 1), (0, 1), (0, 0), (0, 0)],
            order
        );
    }

    #[test]
    fn parallel_doors_board_and_alight_in_the_same_second() {
        let mut doors = Doors::new(DoorOperation::Parallel, 1.0, 1.0);
        let steps = dwell(&mut doors, 2, 2);
        let order: Vec<_> = steps.iter().map(|s| (s.alight, s.board)).collect();
        assert_eq!(vec![(0, 0), (1, 1), (1, 1), (0, 0), (0, 0)], order);
    }

    #[test]
    fn fast_access_lets_several_board_per_second_and_slow_access_waits() {
        let mut fast = Doors::new(DoorOperation::Serial, 0.5, 1.0);
        let boards: Vec<_> = dwell(&mut fast, 0, 3).iter().map(|s| s.board).collect();
        assert_eq!(vec![0, 2, 1, 0, 0], boards);

        let mut slow = Doors::new(DoorOperation::Serial, 2.5, 1.0);
        let steps = dwell(&mut slow, 0, 2);
        let boards: Vec<_> = steps.iter().map(|s| s.board).collect();
        // The second person waits while the first occupies the door, and the doors only close
        // once the last person is through.
        assert_eq!(vec![0, 1, 0, 1, 0, 0, 0, 0], boards);
    }

    #[test]
    fn empty_stop_does_not_open_the_doors() {
        let mut doors = Doors::new(DoorOperation::Serial, 1.0, 1.0);
        assert_eq!(
            DoorStep {
                alight: 0,
                board: 0,
                stop_time: 0.0
            },
            doors.step(0, 0)
        );
    }
}
