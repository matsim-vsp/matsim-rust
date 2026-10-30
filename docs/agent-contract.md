# Agent and Engine Contract

This document states the rules between simulation agents and engines. [architecture.md](architecture.md) describes how
the simulation is built; this document states what agents and engines may rely on and what they must not do.

A change to a rule changes the semantics of the simulation, so it belongs in the same change as the code. Rules are
cited by their number, e.g. `K2`. Rules that the code does not follow yet are listed under
[Implementation status](#implementation-status).

## Terms

- **Agent**: an object that decides what it does next. Usually it represents a person of the population. Agents without
  a person, such as a transit driver, return `None` from `into_person`.
- **Engine**: the physical environment of one part of the simulation. It owns the agents in that environment, moves them
  and applies its rules of time.
- **Phase**: what an agent is doing: an activity, a leg, or being stuck.
- **Owner**: whoever holds the agent value.
- **Cursor**: the current position of an agent in its plan, including its position on the route of the current leg.
- **Decision point**: a call from the owner in which the agent can choose what it does.

## Ownership

- **O1** Every agent has exactly one owner at any time: an engine, a vehicle held by an engine, or `Simulation`.
  `Simulation` only holds agents that are changing between engines and agents that are stuck.
- **O2** An engine only calls engines it contains. Agents move between engines through return values, via the level
  above. There is no reentrant call like `InternalInterface.arrangeNextAgentState` in MATSim.
- **O3** The owned capability in an engine's queue is the permission to be there. The `LegEngine` decides which engine
  simulates a leg; the agent decides which capabilities it offers (see `select_execution`).
- **O4** At the end of the mobsim, every engine returns all agents it owns (`drain`), including those in vehicles. Only
  agents without a person may be dropped.
- **O5** An engine that sends an agent or a vehicle to another partition passes it through the
  `PartitionChangeExtensionsManager`, so that attachments such as backpacks move with it.

## Phases

- **P1** There are three phases: activity, leg and stuck.
- **P2** When a phase ends, the agent determines the next phase. Engines do not know plans. That activities and legs
  alternate is a property of the plan-based agent, not of the simulation.
- **P3** Stuck is final. A stuck agent leaves the simulation, receives no further calls and never changes its phase. It
  stays with `Simulation` until the end of the mobsim and is then materialized like every other agent.
- **P4** An agent becomes stuck in one of two ways: it chooses to when a phase ends, or the mobsim ends while it is not
  in an activity without a next leg. An agent that ended a leg in the last tick therefore counts as stuck.
- **P5** `Simulation` emits exactly one `PersonStuck` event for a stuck agent, in the tick in which it becomes stuck. The
  event carries the link and the mode if the agent was on a leg. The agent's experienced plan is marked as aborted.
- **P6** The stuck phase is unrelated to `qsim.stuck_threshold`. That threshold lets a vehicle leave a link without
  admission capacity; the vehicle and its agents carry on.

## Time

- **T1** A tick runs in a fixed order:
  1. The `ActivityEngine` receives the agents that ended a leg in the previous tick, starts their activities, delivers
     wake-ups and ends the activities that are due.
  2. The `LegEngine` starts the legs of the agents that ended an activity in step 1, moves nodes and links, exchanges
     messages with other partitions and ends the legs that are due.
- **T2** In one tick, an agent passes step 1 and step 2 of `T1` at most once. A chain of elements without duration
  therefore advances by one activity and one leg per tick. It cannot loop within a tick, and no call recurses.

## Knowledge

- **K1** An agent knows its own state and what its trait methods give it: the arguments of the `notify_*` methods and of
  the questions an engine asks. These methods are how knowledge reaches an agent from its environment.
- **K2** In addition, an agent may read immutable scenario data and the responses of services, both through
  `ThreadLocalComputationalEnvironment`.
- **K3** An agent does not read the state of engines, networks or other agents. `ThreadLocalComputationalEnvironment`
  also offers the event managers. Agents do not use them today, but they may in principle. Such a use has to be
  implemented with great care: it must not slow down the processing of events, and events are local to a partition, so
  `K5` and `R1` have to be checked.
- **K4** New knowledge needs a new or extended method on a phase trait. Phase traits have no default methods, so every
  decorator decides explicitly how it handles the new knowledge.
- **K5** What an engine passes to an agent does not depend on the partitioning.

## Decisions and Replanning

- **D1** An agent acts only when its owner calls it. It cannot interrupt its phase on its own, and nothing else can reach
  it.
- **D2** The decision points depend on where the agent is:

  | The agent is ...                 | Decision points                                                |
  |----------------------------------|----------------------------------------------------------------|
  | in an activity                   | `notify_started`, `notify_wakeup`, `notify_finished`           |
  | driving a vehicle on the network | each node, via `notify_move_over_node` and `peek_next_link_id` |
  | being teleported                 | `notify_begin_teleportation`                                   |
  | waiting for a vehicle            | arrival at the stop, wake-ups, each arriving vehicle           |
  | riding in a vehicle with stops   | each stop                                                      |

  The last two rows apply to any engine that lets agents wait or ride.
- **D3** Agents do not change their environment. Engines and `Simulation` emit the events of the simulation; `K3` applies
  to any use of the event managers by an agent. Agents may send requests to services.
- **D4** A wake-up is a single call at a time that the agent requested. The agent names the next time after each
  wake-up. An engine that holds agents at rest offers wake-ups, and an agent without a wake-up costs it nothing.
- **D5** Replanning only changes what lies behind the cursor, which includes the remaining route of the current leg. A
  leg ends earlier than planned only if the owner offers that at a decision point, e.g. by asking a passenger at a stop
  before its planned one.
- **D6** Plan changes apply to the current iteration only. The selected plan in the population stays unchanged: the agent
  works on a copy that it creates at its first change, so that agents that do not replan pay nothing. The score of the
  experienced plan, including the effect of the changes, goes to the selected plan.
- **D7** An agent reads the response of a service only at a simulation time fixed before the response arrives, and
  blocks until it is there. It never polls. Otherwise, the result would depend on how long the service took.

## Determinism

- **R1** Agents at the same time are processed in an order given by stable keys. The order never depends on the arrival
  order of messages or on the iteration order of hash collections.
- **R2** The partitioning does not change when a phase ends or what an agent decides.
- **R3** An engine that sends an entity with a fixed due time to another partition checks on receipt that the due time
  has not passed (see `TeleportationEngine::receive_remote_agent`).
- **R4** Where capacity limits access, e.g. when boarding a vehicle, the arrival tick decides first and the agent id
  second. A passenger who was turned away thus stays ahead of passengers who arrived later.

## Deviations from MATSim

- **Leg end to activity start takes one tick (`T1`).** In QSim, the leg end, the activity start and the end of an activity
  without duration all happen in one call, which is reentrant (`O2`). Here, a leg end is handled in the next tick, because
  this needs no additional code in the top-down step of `T1`. This deviation is intended. As a consequence, activities
  start one tick later than in QSim, and activities with only a maximum duration also end one tick later. Agents reach a
  stop one tick later per leg since their last fixed time. A plan with little slack can thus miss its planned vehicle: in
  the Berlin 0.1 % reference events, 284 of 6,590 teleported transit legs reach their stop after the planned boarding
  time by no more than these ticks.
- **Replanning is not possible from outside (`D1`).** In MATSim, within-day replanning can edit a plan from outside the
  agent between two engine steps. Here, only the agent changes its plan, at a decision point.

## Implementation status

The following rules are not implemented yet:

| Rule         | Current state                                                                                                         |
|--------------|-----------------------------------------------------------------------------------------------------------------------|
| `P1`, `P3`   | `SimulationAgent` has no stuck variant.                                                                               |
| `P4`, `P5`   | Agents become stuck only at the end of the mobsim, in `Simulation::emit_stuck_events`.                                |
| `D2`         | The rows for waiting and riding have no implementation, because schedule-based transit does not exist yet.            |
| `D2`, `D5`   | A route cannot change after its leg has started; `notify_move_over_node` only confirms the planned link.              |
| `D4`         | The `ActivityEngine` calls `notify_wakeup` in every tick for every agent in `awake_q`.                                |
| `D6`         | `replace_next_trip` writes into the selected plan and `into_person` returns it; a test in `agent_logic.rs` expects it. |
