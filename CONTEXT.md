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
