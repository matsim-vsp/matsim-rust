use std::slice::{Iter, IterMut};

/// A simple ring iterator that cycles through a vector of items.
#[derive(Debug)]
pub struct RingIter<T> {
    counter: usize,
    data: Vec<T>,
}

impl<T> RingIter<T> {
    pub fn new(data: Vec<T>) -> Self {
        RingIter { counter: 0, data }
    }

    pub fn next_cloned(&mut self) -> T
    where
        T: Clone,
    {
        let len = self.data.len();
        let client = &mut self.data[self.counter];
        self.counter = (self.counter + 1) % len;
        client.clone()
    }
}

impl<'a, T> IntoIterator for &'a RingIter<T> {
    type Item = &'a T;
    type IntoIter = Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.data.iter()
    }
}

impl<'a, T> IntoIterator for &'a mut RingIter<T> {
    type Item = &'a mut T;
    type IntoIter = IterMut<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.data.iter_mut()
    }
}
