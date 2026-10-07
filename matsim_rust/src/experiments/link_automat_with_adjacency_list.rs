use std::collections::VecDeque;
use std::slice::Iter;

struct Vehicle {
    id: i32,
}

struct Link {
    id: i32,
    q: Q,
    buffer: Q,
}

struct Node {
    id: i32,
    in_links: Vec<usize>,
    out_links: Vec<usize>,
}

type Q = VecDeque<Vehicle>;

pub fn run() {
    // have a network in the form of below. This doesn't require routes yet.
    // 0-\
    //    0---0
    // 0-/
    let vehicle1 = Vehicle { id: 1 };
    let vehicle2 = Vehicle { id: 2 };

    let link1 = Link {
        q: VecDeque::new(),
        buffer: VecDeque::from([vehicle1]),
        id: 1,
    };
    let link2 = Link {
        q: VecDeque::new(),
        buffer: VecDeque::from([vehicle2]),
        id: 2,
    };
    let link3 = Link {
        q: VecDeque::new(),
        buffer: VecDeque::new(),
        id: 3,
    };
    let mut links = vec![link1, link2, link3];

    let node1 = Node {
        id: 1,
        in_links: Vec::new(),
        out_links: Vec::from([0]),
    };
    let node2 = Node {
        id: 2,
        in_links: Vec::new(),
        out_links: Vec::from([1]),
    };
    let node3 = Node {
        id: 3,
        in_links: Vec::from([0, 1]),
        out_links: Vec::from([2]),
    };
    let node4 = Node {
        id: 4,
        in_links: Vec::from([2]),
        out_links: Vec::new(),
    };
    let nodes = vec![node1, node2, node3, node4];

    for i in 0..3 {
        println!("\nstep #{} \n", i);
        move_nodes(nodes.iter(), &mut links);
        move_links(&mut links);
    }
}

fn move_links(links: &mut Vec<Link>) {
    println!("move links");
    for link in links {
        match link.q.pop_front() {
            None => {
                println!("Link #{} has no vehicles in the q", link.id)
            }
            Some(vehicle) => {
                println!(
                    "Pushing vehicle #{} on link #{} from q to buffer",
                    vehicle.id, link.id
                );
                link.buffer.push_back(vehicle);
            }
        }
    }
}

fn move_nodes(nodes: Iter<Node>, links: &mut Vec<Link>) {
    println!("move nodes:");
    for node in nodes {
        for in_link in &node.in_links {
            // we assume that this link is present, because we know the index
            let link = links.get_mut(*in_link).unwrap();

            match link.buffer.pop_front() {
                None => {
                    println!("Link #{} has no vehicles in the buffer", link.id)
                }
                Some(vehicle) => match node.out_links.first() {
                    None => {
                        println!(
                            "Node #{} has no out link. Vehicle #{}'s journey ends",
                            node.id, vehicle.id
                        );
                    }
                    Some(out_link_index) => {
                        let out_link = links.get_mut(*out_link_index).unwrap();
                        println!("Pushing vehicle #{} to link #{}", vehicle.id, out_link.id);
                        out_link.q.push_back(vehicle);
                    }
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::experiments::link_automat_with_adjacency_list::run;

    #[test]
    fn test_run() {
        run();
    }
}
