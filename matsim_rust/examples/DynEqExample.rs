use core::any::Any;
use matsim_rust::simulation::events::{EventTrait, LinkEnterEvent, LinkLeaveEvent};
use matsim_rust::simulation::id::Id;
use matsim_rust::simulation::time::SimTime;
use std::ops::Deref;
// Example for implementation and usage of DynEq, a trait for dynamic equality comparison of trait
// objects. This is used in the implementation of EventTrait to allow for equality comparison of
// different event types.
// Below is a copy of the implementation of DynEq (and subsequent implementation of PartialEq) from
// src/simulation/events/mod.rs

pub trait DynEq: Any {
    fn as_any(&self) -> &dyn Any;
    fn dyn_eq(&self, other: &dyn DynEq) -> bool;
}

impl<T: Any + PartialEq> DynEq for T {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn dyn_eq(&self, other: &dyn DynEq) -> bool {
        if let Some(other) = other.as_any().downcast_ref::<T>() {
            *self == *other
        } else {
            false
        }
    }
}

// Note: in src/simulation/events/mod.rs, we implement PartialEq for dyn EventTrait, which requires
// that EventTrait extends DynEq. For the sake of this example, we implement PartialEq for dyn DynEq
// directly.
impl PartialEq for dyn DynEq {
    fn eq(&self, other: &Self) -> bool {
        self.dyn_eq(other)
    }
}

fn main() {
    // link enter event
    let le: Box<dyn EventTrait> = Box::new(LinkEnterEvent {
        time: SimTime::default(),
        link: Id::create("link"),
        vehicle: Id::create("link"),
        attributes: Default::default(),
    });
    // link leave event
    let ll: Box<dyn EventTrait> = Box::new(LinkLeaveEvent {
        time: SimTime::default(),
        link: Id::create("link"),
        vehicle: Id::create("link"),
        attributes: Default::default(),
    });

    let le_ref = le.deref();
    let ll_ref = ll.deref();

    // check if the two events are equal (should be false as they are of different types)
    dbg!(le_ref == ll_ref);

    // sanity check if the event is equal to itself (should be true)
    dbg!(le_ref == le_ref);

    // ===========

    // checks on more simple types
    let x: &dyn DynEq = &42u32;

    let y = String::from("foo");
    let y: &dyn DynEq = &y;

    let z = String::from("bar");
    let z: &dyn DynEq = &z;

    dbg!(x == x);
    dbg!(x == y);
    dbg!(x == z);

    dbg!(y == x);
    dbg!(y == y);
    dbg!(y == z);

    dbg!(z == x);
    dbg!(z == y);
    dbg!(z == z);
}
