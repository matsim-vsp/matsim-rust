# MATSim simulation

The model describes people travelling through a transport network and using scheduled public transport services.

## Language

**Transit itinerary**:
A passenger's connection from origin to destination, including access, scheduled rides, transfers and egress.
_Avoid_: Vehicle route when referring to the passenger's entire connection.

**Transit ride**:
The part of a passenger's itinerary aboard one transit service between its boarding and alighting stops.

**Transit routing**:
The choice of a passenger's transit itinerary for a requested journey.
_Avoid_: Transit simulation when referring only to itinerary choice.

**Transit execution**:
The movement of transit vehicles and passengers, including waiting, boarding, riding and alighting.
_Avoid_: Transit routing when referring to the execution of an itinerary.

**Car ownership**:
A person's declared ownership of a car. Ownership differs from the existence of a vehicle resource or temporary access to a car.
_Avoid_: Car availability when referring specifically to ownership.

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
