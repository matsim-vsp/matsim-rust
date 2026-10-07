use std::collections::VecDeque;

struct Vehicle {
    id: i32,
}

struct Node {
    id: i32,
    in_link: Option<usize>,
    out_link: Option<usize>,
}

struct Link {
    id: i32,
    queue: VecDeque<Vehicle>,
}

pub fn run() {
    // 1. Put a vehicle in the queue of link1
    let link1: Link = Link {
        queue: VecDeque::from([Vehicle { id: 1 }]),
        id: 1,
    };
    let link2: Link = Link {
        queue: VecDeque::new(),
        id: 2,
    };
    let mut links = vec![link1, link2];

    let node1: Node = Node {
        id: 1,
        in_link: None,
        out_link: Some(0),
    };
    let node2: Node = Node {
        id: 2,
        in_link: Some(0),
        out_link: Some(1),
    };
    let node3: Node = Node {
        id: 3,
        in_link: Some(1),
        out_link: None,
    };
    let nodes = vec![node1, node2, node3];

    // 2. iterate over the nodes. Manually without a loop
    for node in &nodes {
        node_do_sim_step(node, &mut links);
    }
}

fn node_do_sim_step(node: &Node, links: &mut Vec<Link>) {
    // 2.a check for in links.
    match node.in_link {
        None => {
            println!("Node {} has no in link.", node.id)
        }
        Some(in_index) => {
            let in_link = links.get_mut(in_index).unwrap(); // we expect the link to be there if we have a reference to it.
            println!("Node #{} has in link #{}.", node.id, in_link.id);

            match in_link.queue.pop_front() {
                None => {
                    println!("Link {} has no vehicle", in_link.id)
                }
                Some(vehicle) => {
                    println!("Took vehicle #{} from link #{}.", &vehicle.id, in_link.id);

                    match node.out_link {
                        None => {
                            println!("Node #{} has no out link.", node.id)
                        }
                        Some(out_index) => {
                            let out_link = links.get_mut(out_index).unwrap();
                            println!("Node #{} has out link #{}", node.id, out_link.id);

                            println!(
                                "Will push vehicle #{} to link #{}",
                                &vehicle.id, out_link.id
                            );
                            out_link.queue.push_back(vehicle);
                        }
                    }
                }
            }
        }
    }
}
