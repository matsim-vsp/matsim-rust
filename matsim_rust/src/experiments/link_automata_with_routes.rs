use std::collections::VecDeque;
use std::slice::Iter;

type Q = VecDeque<Vehicle>;

struct Vehicle {
    id: u32,
    route: NetworkRoute,
    current_link: usize,
    exit_time: u32,
}

impl Vehicle {
    fn update_route_state(&mut self, current_link: usize, exit_time: u32) {
        self.current_link = current_link;
        self.exit_time = exit_time;
    }
}

struct NetworkRoute {
    link_ids: Vec<usize>,
}

struct Link {
    id: u32,
    q: Q,
}

impl Link {
    fn pop_first_vehicle(&mut self, now: u32) -> Option<Vehicle> {
        match self.q.front() {
            None => None,
            Some(vehicle) => {
                if vehicle.exit_time <= now {
                    self.q.pop_front()
                } else {
                    None
                }
            }
        }
    }

    fn push_vehicle(&mut self, vehicle: Vehicle) {
        self.q.push_back(vehicle);
    }
}

struct Node {
    id: i32,
    in_links: Vec<usize>,
    #[allow(dead_code)] // i want to keep this even though it is never read.
    out_links: Vec<usize>,
}

impl Node {
    fn move_vehicles(&self, links: &mut Vec<Link>, now: u32) {
        for in_link_index in &self.in_links {
            let in_link = links.get_mut(*in_link_index).unwrap();

            match in_link.pop_first_vehicle(now) {
                None => (),
                Some(mut vehicle) => {
                    // increase the pointer into the route's link list by one.
                    let next_route_element = vehicle.current_link + 1;

                    // fetch the next link id of the vehicle's route. If None, the vehicle's trip is over
                    match vehicle.route.link_ids.get(next_route_element) {
                        None => {
                            println!(
                                "Vehicle #{} has reached its destination on link #{}. It will dissapear now. ",
                                vehicle.id, in_link.id
                            )
                        }
                        Some(out_index) => {
                            // fetch the next link of the route.
                            let out_link = links.get_mut(*out_index).unwrap();
                            let out_index_copy = *out_index;

                            // make the transition by increasing the vehicle's pointer into the route array,
                            // updating the exit_time (all vehicles need 2 seconds for each link)
                            // and by pushing the vehicle onto the next link.
                            vehicle.update_route_state(next_route_element, now + 2);
                            out_link.push_vehicle(vehicle);

                            // transition is done. Give a log message with immutable references to the data.
                            let im_in_link = links.get(*in_link_index).unwrap();
                            let im_out_link = links.get(out_index_copy).unwrap();
                            let im_veh = im_out_link.q.back().unwrap();
                            println!(
                                "Pushing vehicle #{} from link #{} to link #{} via node #{}",
                                &im_veh.id, &im_in_link.id, &im_out_link.id, self.id
                            );
                        }
                    }
                }
            }
        }
    }
}

pub fn run() {
    // have a network in the form of below.
    //        0
    //      /  \
    // 0---0    0----0
    //      \  /
    //       0
    //
    // also, have one vehicle travel the upper one the lower route.

    let vehicle1 = Vehicle {
        id: 1,
        route: NetworkRoute {
            link_ids: Vec::from([0, 1, 2, 5]),
        },
        current_link: 0,
        exit_time: 1,
    };
    let vehicle2 = Vehicle {
        id: 2,
        route: NetworkRoute {
            link_ids: Vec::from([0, 3, 4, 5]),
        },
        current_link: 0,
        exit_time: 2,
    };

    let link1 = Link {
        q: VecDeque::from([vehicle1, vehicle2]),
        id: 1,
    };
    let link2 = Link {
        q: VecDeque::new(),
        id: 2,
    };
    let link3 = Link {
        q: VecDeque::new(),
        id: 3,
    };
    let link4 = Link {
        q: VecDeque::new(),
        id: 4,
    };
    let link5 = Link {
        q: VecDeque::new(),
        id: 5,
    };
    let link6 = Link {
        q: VecDeque::new(),
        id: 6,
    };
    let mut links = vec![link1, link2, link3, link4, link5, link6];

    let node1 = Node {
        id: 1,
        in_links: Vec::new(),
        out_links: Vec::from([0]),
    };
    let node2 = Node {
        id: 2,
        in_links: Vec::from([0]),
        out_links: Vec::from([1, 3]),
    };
    let node3 = Node {
        id: 3,
        in_links: Vec::from([1]),
        out_links: Vec::from([2]),
    };
    let node4 = Node {
        id: 4,
        in_links: Vec::from([3]),
        out_links: Vec::from([4]),
    };
    let node5 = Node {
        id: 5,
        in_links: Vec::from([2, 4]),
        out_links: Vec::from([5]),
    };
    let node6 = Node {
        id: 5,
        in_links: Vec::from([5]),
        out_links: Vec::new(),
    };
    let nodes = vec![node1, node2, node3, node4, node5, node6];

    for i in 0..10 {
        println!("\nstep #{}", i);
        move_nodes(nodes.iter(), &mut links, i);
    }
}

fn move_nodes(nodes: Iter<Node>, links: &mut Vec<Link>, now: u32) {
    for node in nodes {
        node.move_vehicles(links, now);
    }
}

#[cfg(test)]
mod tests {
    use crate::experiments::link_automata_with_routes::run;

    #[test]
    fn test_run() {
        run();
    }
}
