use crate::simulation::agents::agent::SimulationAgent;
use crate::simulation::framework_events::QSimId;
use crate::simulation::time::SimTime;
use crate::simulation::vehicles::SimulationVehicle;
use std::any::{Any, type_name};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PartitionChangeContext {
    pub time: SimTime,
    pub from: QSimId,
    pub to: QSimId,
}

#[derive(Clone, Copy)]
pub enum PartitionChangeEntity<'a> {
    Vehicle(&'a SimulationVehicle),
    TeleportationAgent(&'a SimulationAgent),
}

impl std::fmt::Debug for PartitionChangeEntity<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Vehicle(_) => f.write_str("Vehicle"),
            Self::TeleportationAgent(_) => f.write_str("TeleportationAgent"),
        }
    }
}

#[derive(Default)]
pub(crate) struct PartitionChangeAttachments {
    slots: Vec<Option<Box<dyn Any + Send>>>,
}

impl PartitionChangeAttachments {
    fn len(&self) -> usize {
        self.slots.len()
    }
}

impl std::fmt::Debug for PartitionChangeAttachments {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PartitionChangeAttachments")
            .field("slots", &self.slots.len())
            .finish()
    }
}

type SendHandler = dyn for<'a> FnMut(
    PartitionChangeEntity<'a>,
    &PartitionChangeContext,
) -> Option<Box<dyn Any + Send>>;
type ReceiveHandler = dyn for<'a> FnMut(
    PartitionChangeEntity<'a>,
    Option<Box<dyn Any + Send>>,
    &PartitionChangeContext,
);

struct RegisteredHandler {
    send: Box<SendHandler>,
    receive: Box<ReceiveHandler>,
}

#[derive(Default)]
pub struct PartitionChangeExtensionsManager {
    handlers: Vec<RegisteredHandler>,
}

impl std::fmt::Debug for PartitionChangeExtensionsManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PartitionChangeExtensionsManager")
            .field("handlers", &self.handlers.len())
            .finish()
    }
}

impl PartitionChangeExtensionsManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register<T, S, R>(&mut self, mut send: S, mut receive: R)
    where
        T: Any + Send + 'static,
        S: for<'a> FnMut(PartitionChangeEntity<'a>, &PartitionChangeContext) -> Option<T> + 'static,
        R: for<'a> FnMut(PartitionChangeEntity<'a>, Option<T>, &PartitionChangeContext) + 'static,
    {
        self.handlers.push(RegisteredHandler {
            send: Box::new(move |entity, context| {
                send(entity, context).map(|payload| Box::new(payload) as Box<dyn Any + Send>)
            }),
            receive: Box::new(move |entity, payload, context| {
                let payload = payload.map(|payload| {
                    *payload.downcast::<T>().unwrap_or_else(|_| {
                        panic!(
                            "Partition-change attachment has the wrong type; expected {}.",
                            type_name::<T>()
                        )
                    })
                });
                receive(entity, payload, context);
            }),
        });
    }

    pub(crate) fn send(
        &mut self,
        entity: PartitionChangeEntity<'_>,
        context: &PartitionChangeContext,
    ) -> PartitionChangeAttachments {
        let slots = self
            .handlers
            .iter_mut()
            .map(|handler| (handler.send)(entity, context))
            .collect();
        PartitionChangeAttachments { slots }
    }

    pub(crate) fn receive(
        &mut self,
        entity: PartitionChangeEntity<'_>,
        attachments: PartitionChangeAttachments,
        context: &PartitionChangeContext,
    ) {
        assert_eq!(
            attachments.len(),
            self.handlers.len(),
            "Partition-change attachment slot count differs between departure ({}) and arrival ({}).",
            attachments.len(),
            self.handlers.len()
        );

        for (handler, payload) in self.handlers.iter_mut().zip(attachments.slots) {
            (handler.receive)(entity, payload, context);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::create_agent;
    use macros::deterministic_id_test;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[deterministic_id_test]
    fn preserves_slot_order_none_slots_and_owned_payloads() {
        let agent = create_agent(1, vec!["destination"]);
        let context = PartitionChangeContext {
            time: SimTime::from_secs(12),
            from: 0,
            to: 1,
        };
        let received = Rc::new(RefCell::new(Vec::new()));
        let mut manager = PartitionChangeExtensionsManager::new();

        let first = received.clone();
        manager.register::<String, _, _>(
            |_, _| Some("owned".to_owned()),
            move |_, payload, _| first.borrow_mut().push(payload.unwrap()),
        );
        let second = received.clone();
        manager.register::<String, _, _>(
            |_, _| None,
            move |_, payload, _| {
                assert!(payload.is_none());
                second.borrow_mut().push("none".to_owned());
            },
        );

        let attachments = manager.send(PartitionChangeEntity::TeleportationAgent(&agent), &context);
        manager.receive(
            PartitionChangeEntity::TeleportationAgent(&agent),
            attachments,
            &context,
        );

        assert_eq!(&*received.borrow(), &["owned", "none"]);
    }

    #[deterministic_id_test]
    #[should_panic(expected = "slot count differs")]
    fn rejects_wrong_slot_count() {
        let agent = create_agent(1, vec!["destination"]);
        let context = PartitionChangeContext {
            time: SimTime::default(),
            from: 0,
            to: 1,
        };
        let mut manager = PartitionChangeExtensionsManager::new();
        manager.register::<u32, _, _>(|_, _| Some(1), |_, _, _| {});
        manager.receive(
            PartitionChangeEntity::TeleportationAgent(&agent),
            PartitionChangeAttachments::default(),
            &context,
        );
    }

    #[deterministic_id_test]
    #[should_panic(expected = "wrong type")]
    fn rejects_wrong_payload_type() {
        let agent = create_agent(1, vec!["destination"]);
        let context = PartitionChangeContext {
            time: SimTime::default(),
            from: 0,
            to: 1,
        };
        let mut manager = PartitionChangeExtensionsManager::new();
        manager.register::<u32, _, _>(|_, _| Some(1), |_, _, _| {});
        manager.receive(
            PartitionChangeEntity::TeleportationAgent(&agent),
            PartitionChangeAttachments {
                slots: vec![Some(Box::new("not a u32".to_owned()))],
            },
            &context,
        );
    }
}
