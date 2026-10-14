use crate::simulation::id::Id;
use crate::simulation::scenario::transit::{
    TransitDeparture, TransitLine, TransitRoute, TransitStopFacility,
};
use std::collections::BTreeMap;
use std::sync::Mutex;

#[derive(Debug, Clone, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct TransitSegment {
    pub line: Id<TransitLine>,
    pub route: Id<TransitRoute>,
    pub departure: Id<TransitDeparture>,
    pub from: Id<TransitStopFacility>,
    pub to: Id<TransitStopFacility>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct TransitSegmentObservation {
    pub passengers: usize,
    pub capacity: usize,
    pub boarded: usize,
    pub failed_boardings: usize,
}

#[derive(Debug, Default)]
pub(crate) struct TransitCapacityFeedbackCollector(
    Mutex<BTreeMap<TransitSegment, TransitSegmentObservation>>,
);

impl TransitCapacityFeedbackCollector {
    pub fn record(&self, segment: TransitSegment, observation: TransitSegmentObservation) {
        let mut observations = self.0.lock().expect("transit feedback collector poisoned");
        let current = observations.entry(segment).or_default();
        current.passengers = current.passengers.saturating_add(observation.passengers);
        current.capacity = current.capacity.saturating_add(observation.capacity);
        current.boarded = current.boarded.saturating_add(observation.boarded);
        current.failed_boardings = current
            .failed_boardings
            .saturating_add(observation.failed_boardings);
    }

    pub fn take(&self) -> BTreeMap<TransitSegment, TransitSegmentObservation> {
        std::mem::take(&mut *self.0.lock().expect("transit feedback collector poisoned"))
    }
}

#[cfg(test)]
mod tests {
    use super::{TransitCapacityFeedbackCollector, TransitSegment, TransitSegmentObservation};
    use crate::simulation::id::Id;
    use macros::deterministic_id_test;

    fn segment() -> TransitSegment {
        TransitSegment {
            line: Id::create("line"),
            route: Id::create("route"),
            departure: Id::create("departure"),
            from: Id::create("from"),
            to: Id::create("to"),
        }
    }

    #[deterministic_id_test]
    fn worker_observations_merge_repeatably_and_reset_after_publication() {
        let first = TransitSegmentObservation {
            passengers: 3,
            capacity: 4,
            boarded: 1,
            failed_boardings: 2,
        };
        let second = TransitSegmentObservation {
            passengers: 5,
            capacity: 6,
            boarded: 3,
            failed_boardings: 4,
        };

        let collect = |observations| {
            let collector = TransitCapacityFeedbackCollector::default();
            for observation in observations {
                collector.record(segment(), observation);
            }
            collector
        };
        let merged = collect([first, second]).take();
        assert_eq!(
            Some(&TransitSegmentObservation {
                passengers: 8,
                capacity: 10,
                boarded: 4,
                failed_boardings: 6,
            }),
            merged.get(&segment())
        );
        assert_eq!(merged, collect([second, first]).take());

        let collector = TransitCapacityFeedbackCollector::default();
        collector.record(segment(), first);
        assert_eq!(1, collector.take().len());
        assert!(collector.take().is_empty());
    }
}
