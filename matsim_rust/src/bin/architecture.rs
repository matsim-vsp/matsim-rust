trait Agent {
    fn name(&self) -> &str;
}

struct AgentImpl;

impl Agent for AgentImpl {
    fn name(&self) -> &str {
        "AgentImpl"
    }
}

struct WorkFromHome;

// impl WorkFromHome {
//     fn turn_on_laptop(&self, agent: &dyn Agent) {
//         println!("Agent {} is turning on their laptop.", agent.name());
//         agent.open_laptop();
//         agent.login_pc();
//     }
// }
//
// struct WorkFromOffice;
//
// impl WorkFromOffice {
//     fn turn_on_pc(&self, agent: &dyn Agent) {
//         println!("Agent {} is turning on their laptop.", agent.name());
//         agent.login_pc();
//     }
// }

fn main() {
    print!("Hello, world!");
}
