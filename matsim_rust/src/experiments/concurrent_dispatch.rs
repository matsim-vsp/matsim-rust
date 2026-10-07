use rand::RngExt;
use std::sync::mpsc;
use std::sync::mpsc::{Receiver, Sender};
use std::thread;

/** This should run send ping pong messages between multiple nodes. The structure looks like the following

    0    1
     \  /
      4
     / \
    2   3

 Nodes 0,1,3,4 can send messages to 2 and 2 can send messages to any other node. So, each node has to
 have a Sender to 2, and 2 has to have a Sender to all the other nodes.

 This example is to figure out how to wire up the channels.

 Notes1: The senders must be dropped on the main thread so that the ref-counter reaches 0 once the
 Dispatcher drops all the senders when it goes out of scope.

 Note2: Currently the dispatcher-thread decides that it wants to end execution. All the sending-ends
        it holds are dropped and the relay threads receive an error while waiting for receiver.recv.
        Alternatively one could introduce some 'Terminate' message, which lets the
        other parties terminate. To avoid doing regular program flow with an error. - Don't know whether
        that is a valid thing to do in Rust yet -.
*/
fn run() {
    let runners = create_runners();

    // now start the threads
    let mut handles = Vec::new();
    for runner in runners {
        let handle = match runner {
            Runner::Relay(relay) => thread::spawn(move || relay.run()),
            Runner::Dispatcher(dispatcher) => thread::spawn(move || dispatcher.run()),
        };
        handles.push(handle);
    }

    // wait for all threads to finish.
    for handle in handles {
        handle.join().unwrap();
    }
}

/** This seems to be rather complicated to me. Because of the ownership rules I have separated everything
    into separate arrays, which have to align their items by index. This works but doesn't feel very
    ergonomic. I don't have a better idea of how to do this yet when things in the same collection
    have to reference other items from the very same collection.
*/
fn create_runners() -> Vec<Runner> {
    // specify the ids of the nodes
    let ids: [usize; 5] = [0, 1, 2, 3, 4];
    let dispatcher_index = 4;

    // create nodes and senders
    let mut nodes = Vec::new();
    let mut senders = Vec::new();

    for id in ids {
        let channel = mpsc::channel();
        let node = Node::new(id, channel.1);
        nodes.push(node);
        senders.push(channel.0);
    }

    let mut runners = Vec::new();
    for node in nodes {
        // create relays
        if node.id != dispatcher_index {
            let sender = senders.get(dispatcher_index).unwrap().clone();
            let connection = Connection {
                target_id: dispatcher_index,
                sender,
            };
            let relay = Relay { node, connection };
            runners.push(Runner::Relay(relay));
        } else {
            // create dispatcher
            let connections: Vec<Connection> = ids
                .iter()
                .map(|id| {
                    let sender = senders.get(*id).unwrap().clone();
                    Connection {
                        target_id: *id,
                        sender,
                    }
                })
                .collect();

            let dispatcher = Dispatcher { node, connections };
            runners.push(Runner::Dispatcher(dispatcher));
        }
    }
    runners
}

enum Runner {
    Relay(Relay),
    Dispatcher(Dispatcher),
}

struct Dispatcher {
    node: Node,
    connections: Vec<Connection>,
}

impl Dispatcher {
    /** This method consumes self, so that this runner can only be run once. This way all the fields
        which belong to this struct are dropped at the end of this call. This includes the sending-ends
        of the channels. This way the waiting threads of the receiving ends receive an error and can
        terminate themselves as well.
    */
    fn run(self) {
        let message = Message {
            counter: 0,
            message: format!("From Dispatcher {}", self.node.id),
        };

        // send the first message
        self.send_to_random_relay(message);

        loop {
            match self.node.receiver.recv() {
                Ok(mut message) => {
                    println!("Dispatcher {} received {message:#?}", self.node.id);

                    if message.counter > 5 {
                        println!(
                            "Dispatcher {} has received message with counter > 5. Breaking out of the loop.",
                            self.node.id
                        );
                        break;
                    }

                    message.message = format!("From Dispatcher {}", self.node.id);
                    message.counter = message.counter + 1;

                    self.send_to_random_relay(message);
                }
                Err(_) => {
                    println!("Dispatcher {} received an error on recv.", self.node.id);
                    break;
                }
            }
        }

        println!("Dispatcher {} is finishing.", self.node.id);

        println!("Dispatcher has dropped all connections.")
    }

    fn send_to_random_relay(&self, message: Message) {
        let target: usize = rand::rng().random_range(0..4);
        let connection = self.connections.get(target).unwrap();

        println!("Dispatcher {} sending {message:#?}", self.node.id);
        connection.sender.send(message).unwrap();
    }
}

struct Relay {
    node: Node,
    connection: Connection,
}

impl Relay {
    fn run(self) {
        println!("Relay {} started.", self.node.id);
        loop {
            // wait for incoming messages
            match self.node.receiver.recv() {
                Ok(mut message) => {
                    println!("Relay {} received {message:#?}", self.node.id);

                    message.message = format!("From Relay {}", self.node.id);
                    message.counter = message.counter + 1;

                    println!("Relay {} sending {message:#?}", self.node.id);
                    self.connection.sender.send(message).unwrap();
                }
                Err(_) => {
                    println!("Relay {} received an error on recv", self.node.id);
                    break;
                }
            }
        }
        println!("Relais {} is finishing.", self.node.id);
    }
}

struct Node {
    id: usize,
    receiver: Receiver<Message>,
}

impl Node {
    fn new(id: usize, receiver: Receiver<Message>) -> Self {
        Self { id, receiver }
    }
}

struct Connection {
    target_id: usize,
    sender: Sender<Message>,
}

#[derive(Debug)]
struct Message {
    counter: u32,
    message: String,
}

#[cfg(test)]
mod tests {
    use crate::experiments::concurrent_dispatch::run;

    #[test]
    fn test_run() {
        run();
    }
}
