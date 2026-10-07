use crate::simulation::Identifiable;
use crate::simulation::agents::EndTime;
use crate::simulation::id::Id;
use crate::simulation::id::serializable_type::StableTypeId;
use crate::simulation::time::SimTime;
use nohash_hasher::IntMap;
use std::cmp::Ordering;
use std::collections::BinaryHeap;

struct Entry<T> {
    end_time: SimTime,
    order: u64,
    value: T,
}

impl<T> PartialEq<Self> for Entry<T> {
    fn eq(&self, other: &Self) -> bool {
        self.end_time == other.end_time && self.order == other.order
    }
}

impl<T> Eq for Entry<T> {}

impl<T> PartialOrd<Self> for Entry<T> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<T> Ord for Entry<T> {
    fn cmp(&self, other: &Self) -> Ordering {
        // First compare by end_time (reverse for min-heap)
        // Then use order as secondary sort key (also reverse for FIFO within same time)
        other
            .end_time
            .cmp(&self.end_time)
            .then_with(|| other.order.cmp(&self.order))
    }
}

/// TimeQueue provides a priority queue ordered by time with stable FIFO ordering
/// for entries with the same time.
///
/// Note: The internal counter will wrap around after u64::MAX insertions.
/// This is acceptable for simulation purposes as it would take an astronomically large number
/// of insertions to overflow, and wrapping would only affect ordering in the unlikely event
/// of having entries with both the same time and counter values after overflow.
pub struct TimeQueue<T, I>
where
    T: EndTime,
{
    q: BinaryHeap<Entry<T>>,
    counter: u64,
    _phantom: std::marker::PhantomData<I>,
}

impl<T, I> Default for TimeQueue<T, I>
where
    T: EndTime,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<T, I> TimeQueue<T, I>
where
    T: EndTime,
{
    pub fn new() -> Self {
        TimeQueue {
            q: BinaryHeap::new(),
            counter: 0,
            _phantom: std::marker::PhantomData,
        }
    }

    pub fn add(&mut self, value: T, now: SimTime) {
        let order = self.counter;
        self.counter = self.counter.wrapping_add(1);
        self.add_with_order(value, now, order);
    }

    /// Adds a value using an explicit tie-break order for entries with the same end time.
    /// Callers must provide unique, stable order values for their entries.
    pub fn add_with_order(&mut self, value: T, now: SimTime, order: u64) {
        let end_time = value.end_time(now);
        self.q.push(Entry {
            end_time,
            order,
            value,
        });
    }

    pub fn pop(&mut self, now: SimTime) -> Vec<T> {
        let mut result: Vec<T> = Vec::new();

        while let Some(entry_ref) = self.q.peek() {
            if entry_ref.end_time <= now {
                let entry = self.q.pop().unwrap();
                result.push(entry.value);
            } else {
                break;
            }
        }

        result
    }

    pub fn drain(&mut self) -> Vec<T> {
        self.q.drain().map(|entry| entry.value).collect()
    }

    #[cfg(test)]
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        self.q.len()
    }
}

#[allow(dead_code)]
struct ValueWrap<I: StableTypeId> {
    id: Id<I>,
    end_time: SimTime,
}

impl<I: StableTypeId> ValueWrap<I> {
    #[allow(dead_code)]
    fn new(id: Id<I>, end_time: SimTime) -> Self {
        ValueWrap { id, end_time }
    }
}

impl<I: StableTypeId> EndTime for ValueWrap<I> {
    fn end_time(&self, _now: SimTime) -> SimTime {
        self.end_time
    }
}

/// This is a mutable version of TimeQueue. It allows to mutate the values in the queue.
/// It is a logical error to mutate the end_time of the value such that the order of the queue is changed.
/// TODO taxi driver needs to be able to change his end_time such that order is changed
#[allow(dead_code)]
pub struct MutTimeQueue<T, I>
where
    T: EndTime + Identifiable<I>,
    I: StableTypeId,
{
    q: TimeQueue<ValueWrap<I>, I>,
    cache: IntMap<Id<I>, T>,
}

impl<T, I> Default for MutTimeQueue<T, I>
where
    T: EndTime + Identifiable<I>,
    I: StableTypeId + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

#[allow(dead_code)]
impl<T, I> MutTimeQueue<T, I>
where
    T: EndTime + Identifiable<I>,
    I: StableTypeId + 'static,
{
    pub fn new() -> Self {
        MutTimeQueue {
            q: TimeQueue::new(),
            cache: IntMap::default(),
        }
    }

    pub fn add(&mut self, value: T, now: SimTime) {
        let id = value.id();
        self.q
            .add(ValueWrap::new(id.clone(), value.end_time(now)), now);
        self.cache.insert(id.clone(), value);
    }

    pub fn pop(&mut self, now: SimTime) -> Vec<T> {
        let ids = self.q.pop(now);
        let mut result: Vec<T> = Vec::new();

        for id in ids {
            let value = self.cache.remove(&id.id).unwrap();
            result.push(value);
        }

        result
    }

    pub(crate) fn iter_mut(&mut self) -> impl Iterator<Item = &mut T> {
        self.cache.values_mut()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone)]
    struct TestItem {
        id: u32,
        end: SimTime,
    }

    impl EndTime for TestItem {
        fn end_time(&self, _now: SimTime) -> SimTime {
            self.end
        }
    }

    #[test]
    fn test_time_queue_stable_ordering() {
        let mut queue: TimeQueue<TestItem, ()> = TimeQueue::new();
        queue.add(
            TestItem {
                id: 1,
                end: SimTime::from_secs(10),
            },
            SimTime::from_secs(0),
        );
        queue.add(
            TestItem {
                id: 2,
                end: SimTime::from_secs(10),
            },
            SimTime::from_secs(0),
        );
        queue.add(
            TestItem {
                id: 3,
                end: SimTime::from_secs(10),
            },
            SimTime::from_secs(0),
        );

        let results = queue.pop(SimTime::from_secs(10));
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].id, 1);
        assert_eq!(results[1].id, 2);
        assert_eq!(results[2].id, 3);
    }

    #[test]
    fn test_time_queue_explicit_order_is_independent_of_insertion_order() {
        let mut queue: TimeQueue<TestItem, ()> = TimeQueue::new();
        for id in [3, 1, 2] {
            queue.add_with_order(
                TestItem {
                    id,
                    end: SimTime::from_secs(10),
                },
                SimTime::from_secs(0),
                u64::from(id),
            );
        }

        let ids: Vec<_> = queue
            .pop(SimTime::from_secs(10))
            .into_iter()
            .map(|item| item.id)
            .collect();
        assert_eq!(ids, vec![1, 2, 3]);
    }

    #[test]
    fn test_time_queue_time_ordering_priority() {
        let mut queue: TimeQueue<TestItem, ()> = TimeQueue::new();
        queue.add(
            TestItem {
                id: 1,
                end: SimTime::from_secs(15),
            },
            SimTime::from_secs(0),
        );
        queue.add(
            TestItem {
                id: 2,
                end: SimTime::from_secs(10),
            },
            SimTime::from_secs(0),
        );
        queue.add(
            TestItem {
                id: 3,
                end: SimTime::from_secs(20),
            },
            SimTime::from_secs(0),
        );
        queue.add(
            TestItem {
                id: 4,
                end: SimTime::from_secs(10),
            },
            SimTime::from_secs(0),
        );

        let results = queue.pop(SimTime::from_secs(10));
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].id, 2);
        assert_eq!(results[1].id, 4);

        let results = queue.pop(SimTime::from_secs(15));
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, 1);

        let results = queue.pop(SimTime::from_secs(20));
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, 3);
    }

    #[test]
    fn test_time_queue_subsecond_due_time() {
        let mut queue: TimeQueue<TestItem, ()> = TimeQueue::new();
        let due_time = SimTime::from_nanos(350_000_000);
        queue.add(
            TestItem {
                id: 1,
                end: due_time,
            },
            SimTime::from_nanos(0),
        );

        let early = queue.pop(SimTime::from_nanos(349_000_000));
        assert!(early.is_empty());

        let ready = queue.pop(due_time);
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id, 1);
    }
}
