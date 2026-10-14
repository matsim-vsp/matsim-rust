# Target Java transit behavior for the SwissRailRaptor work

The transit work targets a faithful port of Java SwissRailRaptor rather than only a RAPTOR-based replacement for the current router. Matching Java passenger itineraries is the first priority because itinerary selection determines demand at stations and on services. The requested scope also includes transit execution compatibility; timetable-driven vehicle execution is a separate component from SwissRailRaptor and needs its own validation.

The feature target includes the optional SwissRailRaptor capabilities provided by the pinned Java reference, delivered in stages with explicit coverage. Acceptance requires running shared fixtures in Java and Rust and comparing chosen services, transfers, boarding and alighting stops, times and passenger outcomes, including single- and multi-partition execution. Rust-only tests do not establish compatibility.

The reference is MATSim 2026.0, commit `c7a75ebeddc3ceb62959af046190064bf23770df`. Tagged source defines behavior; historical documentation does not override it. No existing Java scenario was supplied.

Execution scope includes ordinary queue-based transit and the SBB timetable-driven transit engine. The separate detailed `railsim` extension is outside this work.

For random route choices, Rust keeps its deterministic RNG contract. Differential tests compare unique optimal itineraries directly; random choices are an intentional compatibility deviation and need separate behavioral checks rather than assuming identical Java random streams.

PT-to-car fallback is permitted only when the person's boolean `ownsCar` attribute is true. Missing or false prohibits fallback. Generated person vehicles do not establish ownership. This is an intentional application policy beyond the Java routing reference. If no PT connection exists for an agent without a car, use the pinned Java router's walk-only behavior where applicable; otherwise return an explicit no-path result.

Personless PT queries also return PT, walk-only or no-path results. Any required SILO car fallback remains an explicit, separate application policy and must not become a passenger's PT itinerary.

Differential comparisons cover itineraries, passenger outcomes, vehicle loads and meaningful event sequences. Independent events at the same timestamp may be reordered; dependencies that affect passenger handling or capacity must be preserved. Numerical tolerances are specified per metric rather than requiring identical event-file bytes.

The implementation proposal is in [the SwissRailRaptor plan](../swissrailraptor-plan.md). Concrete numerical tolerances follow the pinned algorithms and the simulation clock and must not hide behavioral differences. This decision records the compatibility target, not approval of an implementation design or a claim of full Java MATSim parity.
