use std::cell::RefCell;
use std::rc::{Rc, Weak};

enum State {
    ACTIVITY,
    TELEPORTATION,
}

struct Agent {
    id: String,
    state: State,
}

impl Agent {
    fn new(id: String, state: State) -> Self {
        Agent { id, state }
    }
}

struct InternalInterface {
    activity_engine: Rc<RefCell<dyn Engine>>,
    teleportation_engine: Rc<RefCell<dyn Engine>>,
}

impl InternalInterface {
    fn arrange_next_agent_state(&self, agent: Agent) {
        match agent.state {
            State::ACTIVITY => self.activity_engine.borrow_mut().receive_agent(agent),
            State::TELEPORTATION => self.teleportation_engine.borrow_mut().receive_agent(agent),
        }
    }
}

struct Simulation {
    engines: Vec<Rc<RefCell<dyn Engine>>>,
    internal_interface: Rc<RefCell<InternalInterface>>,
}

impl Simulation {
    fn new(
        engines: Vec<Rc<RefCell<dyn Engine>>>,
        internal_interface: Rc<RefCell<InternalInterface>>,
    ) -> Self {
        Simulation {
            internal_interface,
            engines,
        }
    }

    fn run(&mut self) {
        let mut now = 0;
        while now < 20 {
            for engine in &self.engines {
                engine.borrow_mut().do_step(now);
            }
            now += 1;
        }
    }
}

trait Engine {
    fn do_step(&mut self, now: u32);
    fn set_internal_interface(&mut self, internal_interface: Weak<RefCell<InternalInterface>>);
    fn receive_agent(&mut self, agent: Agent);
}

struct ActivityEngine {
    agents: Vec<Agent>,
    //to prevent memory leaks, we use Weak instead of Rc (https://doc.rust-lang.org/book/ch15-06-reference-cycles.html)
    internal_interface: Weak<RefCell<InternalInterface>>,
}

impl Engine for ActivityEngine {
    fn do_step(&mut self, now: u32) {
        if now % 10 == 0 {
            println!("Activity engine: Time step {}, stop activity", now);
            self.agents.get_mut(0).unwrap().state = State::TELEPORTATION;
            self.internal_interface
                .upgrade()
                .unwrap()
                .borrow_mut()
                .arrange_next_agent_state(self.agents.remove(0))
        } else {
            // println!("Activity engine: Time step {}, doing nothing", now)
        }
    }

    fn set_internal_interface(&mut self, internal_interface: Weak<RefCell<InternalInterface>>) {
        self.internal_interface = internal_interface;
    }

    fn receive_agent(&mut self, agent: Agent) {
        println!("Activity engine: Received agent");
        self.agents.push(agent);
    }
}

struct TeleportationEngine {
    agents: Vec<Agent>,
    //to prevent memory leaks, we use Weak instead of Rc (https://doc.rust-lang.org/book/ch15-06-reference-cycles.html)
    internal_interface: Weak<RefCell<InternalInterface>>,
}

impl Engine for TeleportationEngine {
    fn do_step(&mut self, now: u32) {
        if now % 10 == 5 {
            println!(
                "Teleportation engine: Time step {}, stop teleportation",
                now
            );
            self.agents.get_mut(0).unwrap().state = State::ACTIVITY;
            self.internal_interface
                .upgrade()
                .unwrap()
                .borrow_mut()
                .arrange_next_agent_state(self.agents.remove(0))
        } else {
            // println!("Teleportation engine: Time step {}, doing nothing", now)
        }
    }

    fn set_internal_interface(&mut self, internal_interface: Weak<RefCell<InternalInterface>>) {
        self.internal_interface = internal_interface;
    }

    fn receive_agent(&mut self, agent: Agent) {
        println!("Teleportation engine: Received agent");
        self.agents.push(agent);
    }
}

#[cfg(test)]
mod tests {
    use crate::experiments::internal_interface::{
        ActivityEngine, Agent, Engine, InternalInterface, Simulation, State, TeleportationEngine,
    };
    use std::cell::RefCell;
    use std::rc::{Rc, Weak};

    #[test]
    fn test_run() {
        let activity_engine: Rc<RefCell<dyn Engine>> = Rc::new(RefCell::new(ActivityEngine {
            agents: vec![Agent::new(String::from("agent"), State::ACTIVITY)],
            internal_interface: Weak::new(),
        }));
        let teleportation_engine: Rc<RefCell<dyn Engine>> =
            Rc::new(RefCell::new(TeleportationEngine {
                agents: Vec::new(),
                internal_interface: Weak::new(),
            }));
        let internal_interface = Rc::new(RefCell::new(InternalInterface {
            activity_engine: Rc::clone(&activity_engine),
            teleportation_engine: Rc::clone(&teleportation_engine),
        }));

        activity_engine
            .borrow_mut()
            .set_internal_interface(Rc::downgrade(&internal_interface));
        teleportation_engine
            .borrow_mut()
            .set_internal_interface(Rc::downgrade(&internal_interface));

        let mut sim = Simulation::new(
            vec![activity_engine, teleportation_engine],
            internal_interface,
        );

        sim.run();
    }
}
