use std::collections::VecDeque;
use std::slice::IterMut;

struct Vehicle {
    id: i32,
}

struct Link {
    main_q: VecDeque<Vehicle>,
    buffer: VecDeque<Vehicle>,
}

type Q = VecDeque<Vehicle>;

pub fn run() {
    let mut link1 = Link {
        main_q: VecDeque::new(),
        buffer: VecDeque::new(),
    };
    link1.buffer.push_back(Vehicle { id: 1 });
    let mut link2 = Link {
        main_q: VecDeque::new(),
        buffer: VecDeque::new(),
    };

    //let mut node1_buffer: VecDeque<Vehicle> = VecDeque::new();
    // let mut node3_main_q: VecDeque<Vehicle> = VecDeque::new();
    println!(
        "Starting with {} vehicles on link1's main_q and {} vehicles on link1's buffer",
        link1.main_q.len(),
        link1.buffer.len()
    );
    println!(
        "Starting with {} vehicles on link2's main_q and {} vehicles on link2's buffer",
        link2.main_q.len(),
        link2.buffer.len()
    );

    let node1: (Option<&mut Q>, Option<&mut Q>, i32) = (None, Some(&mut link1.main_q), 1);
    let node2: (Option<&mut Q>, Option<&mut Q>, i32) =
        (Some(&mut link1.buffer), Some(&mut link2.main_q), 2);
    let node3: (Option<&mut Q>, Option<&mut Q>, i32) = (Some(&mut link2.buffer), None, 3);
    let mut nodes = vec![node1, node2, node3];

    move_nodes(nodes.iter_mut());

    println!(
        "After 1st move_nodes {} vehicles on link1's main_q and {} vehicles on link1's buffer",
        link1.main_q.len(),
        link1.buffer.len()
    );
    println!(
        "After 1st move_nodes {} vehicles on link2's main_q and {} vehicles on link2's buffer",
        link2.main_q.len(),
        link2.buffer.len()
    );

    // this will be move_links
    let veh = link2.main_q.pop_front().unwrap();
    link2.buffer.push_back(veh);

    // now we run into problems with the borrow checker, because if we re-use the nodes we will have two
    // mutable references to link2's buffer and main_q :-(
    // move_nodes(nodes.iter_mut());
}

fn move_nodes(nodes_iter: IterMut<(Option<&mut Q>, Option<&mut Q>, i32)>) {
    for node in nodes_iter {
        match &mut node.0 {
            None => {
                println!("Node #{} has no in link", &node.2)
            }
            Some(buffer) => {
                println!("Node #{} has a buffer reference", &node.2);
                match buffer.pop_front() {
                    None => {
                        println!("In-buffer of Node #{} has no vehicle", &node.2)
                    }
                    Some(vehicle) => {
                        println!(
                            "Took vehicle #{} from in-buffer of node #{}.",
                            &vehicle.id, &node.2
                        );
                        match &mut node.1 {
                            None => {
                                println!(
                                    "Node #{} has no out-q. Vehicle #{}'s journey ends here",
                                    &node.2, &vehicle.id
                                )
                            }
                            Some(in_q) => {
                                println!(
                                    "Node #{} has out-q. Will push vehicle #{} onto it.",
                                    &node.2, &vehicle.id
                                );
                                in_q.push_back(vehicle);
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::experiments::link_automat_with_references::run;

    #[test]
    fn test_run() {
        run()
    }
}
