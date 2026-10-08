# Simulation analysis

Simulation analysis describes a simulated run and presents its outcomes for human review.

## Language

**Visualization report**:
A presentation of simulation results whose primary content is charts and maps. Its acceptance depends on whether a person can understand and assess the results.
_Avoid_: Raw-data report

**Simulation metric**:
A quantitative description of a simulated run, such as travel duration, journey completion, network volume, or transit occupancy.
_Avoid_: Acceptance criterion

## Run outcomes

**Stranded agent**:
An agent whose vehicle was removed because it was stuck. Its current leg was abandoned rather than made, so it has no arrival.
_Avoid_: Completed journey, teleported agent

**Abandoned leg**:
The leg a stranded agent was on. It is never made, so it contributes no arrival and no travelled distance, and it is not evidence that the agent travelled.
_Avoid_: Completed leg, failed journey

**Resumed agent**:
A stranded agent that starts the activity following its abandoned leg at that activity's own scheduled time. Resuming moves the agent's plan forward; it does not move the agent to where it was going.
_Avoid_: Teleported agent, recovered journey
