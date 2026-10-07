# Analyses to run after MATSim

Research date: 5 October 2026.

The integrated specification is [Simulation result analysis requirements](simulation-analysis-requirements.md). It combines the user's hourly link and agent analyses with the research findings below, including validation, stability, scenario comparisons, uncertainty and conditional transit, accessibility, equity, appraisal and environmental modules. This document retains the research evidence and its limits.

The selected execution scope is one analysis after each successful run, using only its latest completed iteration. Iteration-stability findings below remain research context and are excluded from the implementation requirements.

Transportation researchers need evidence that the run is usable, that the baseline represents observed travel, and that the reported outcomes answer their research question. There is no universal mandatory list of MATSim analyses. The priorities below are a synthesis of official documentation, analysis implementations, and selected research applications.

This is a targeted source review, not a systematic review or a survey of how frequently researchers use each metric. Case studies establish what those authors analyzed. Recommendations and the proposed module order are our interpretation of that evidence.

## Core analysis package

| Priority | Analysis | What to report | Inputs |
| --- | --- | --- | --- |
| 1 | Run integrity | Planned, departed, completed and unfinished journeys; stuck persons by location and time; unmatched events; warnings and errors | Events, input population, logs, run configuration |
| 1 | Iteration stability | Scores where meaningful, mode shares, total travel time, vehicle distance and key link volumes across later iterations | Iteration outputs, scores, events |
| 1 | Baseline validation | Observed versus simulated road volumes, travel times or speeds, survey mode shares and trip distributions; transit demand where relevant | Simulated metrics plus independently observed data |
| 2 | Travel demand and behavior | Journey counts and mode shares, distance and duration distributions, departure profiles, activity purpose and trips per person | Reconstructed journeys, population attributes, network |
| 2 | Network performance | Link volumes and travel times by time interval, vehicle kilometres and hours, delay relative to free flow, congestion maps | Vehicle/link events, network lengths and speeds |
| 2 | Scenario comparison | Baseline and alternatives, absolute and relative changes, spatial differences and affected traveler groups | Consistent outputs for each scenario |
| 2 | Uncertainty and sensitivity | Variation between seeds; sensitivity to key behavioral, demand and capacity assumptions | Repeated runs, parameter manifest, common metrics |

Priority 1 means these checks should precede substantive interpretation. Priority 2 describes a useful general reporting package. The exact metrics depend on the simulated modes and the research claim.

### Run integrity and iteration stability

The official guide describes warnings/error logs, iteration score histories, travel-distance statistics, departure/arrival histograms, trip durations, and hourly link statistics. Its counts chapter describes observed versus simulated hourly volumes and population-sample scaling. These outputs support diagnostics and validation. [MATSim User Guide, sections 2.3 and 11.4](https://matsim.org/files/book/partOne-latest.pdf).

MATSim maintainers warn that incomplete journeys can distort analysis and scenario comparison. Experienced plans reconstructed from events can omit some persons. An absent person is not necessarily a failed traveler, since someone staying home may generate no travel events. [Maintainer discussion of stuck agents](https://github.com/matsim-org/matsim-libs/issues/3008).

Recommended practice:

- Keep completed, unfinished, and non-traveling persons distinct. Publish failure counts and an explicit inclusion rule.
- Inspect where failures occur and whether the simulation ends before travelers finish.
- Check several outcomes over later iterations. A flat mean score alone does not establish empirical validity or stability of every link.
- For comparisons that exclude failed travelers, use a consistent comparison population and report exclusions. Also report failures as an outcome, especially if the intervention causes them.

### Baseline validation

In the New York City application, He and colleagues calibrated road speeds and East River crossing traffic volumes, validated transit-station demand and selected road-link counts, then evaluated congestion pricing. This demonstrates the value of checking multiple observed quantities before policy interpretation. Their numerical errors are results for that application, not universal acceptance criteria. [He et al., Transport Policy, 2021](https://arxiv.org/abs/2008.04762).

MATSim's current count-comparison implementation produces hourly and daily comparisons, error summaries and quality categories, including GEH statistics. [CountComparisonAnalysis source](https://raw.githubusercontent.com/matsim-org/matsim-libs/main/contribs/application/src/main/java/org/matsim/application/analysis/traffic/CountComparisonAnalysis.java).

Recommended validation deliverables are observed-versus-simulated scatterplots, hourly profiles, residual maps and an error table. Match vehicle classes, geography, observation dates and time intervals. Distinguish observations used for calibration from held-out validation where possible. State the source and uncertainty of each observation dataset.

For sampled populations, document expansion weights and capacity scaling separately. Multiplying counts to full-population totals does not by itself establish that congestion dynamics match a full-population run. A dedicated MATSim downscaling study examines this issue. [Ben-Dor et al., Simulation Modelling Practice and Theory, 2021](https://doi.org/10.1016/j.simpat.2020.102233).

### Travel demand and network performance

MATSim's trip analysis includes mode shares by distance and purpose, trip statistics, departure profiles by purpose, trips per person, mode chains and shifts. It consumes trip and person tables. [TripAnalysis source](https://raw.githubusercontent.com/matsim-org/matsim-libs/main/contribs/application/src/main/java/org/matsim/application/analysis/population/TripAnalysis.java).

A journey can contain several legs. For example, walk, transit and walk legs can constitute one transit journey. MATSim explicitly distinguishes analysis main mode from routing mode identification. Counting legs as independent journeys produces a different mode-share denominator. [AnalysisMainModeIdentifier documentation](https://www.matsim.org/doxygen/interfaceorg_1_1matsim_1_1core_1_1router_1_1_analysis_main_mode_identifier.html).

The traffic analysis implementation calculates congestion indices and relative travel times, with outputs by link, road type and hour. [TrafficAnalysis source](https://raw.githubusercontent.com/matsim-org/matsim-libs/main/contribs/application/src/main/java/org/matsim/application/analysis/traffic/TrafficAnalysis.java).

For a general report, include medians and upper percentiles alongside means. Separate passenger travel from vehicle travel. Explain how link lengths, departure/arrival links and teleported modes contribute to distance. Include maps and peak-hour breakdowns, since a daily total can conceal congestion moving to another corridor or time.

### Scenario comparisons and uncertainty

The NYC study reports car-trip changes and travel consumer-surplus effects for different population segments. Its policy conclusions depend on who benefits and who loses, as well as citywide totals. [He et al.](https://arxiv.org/abs/2008.04762).

A Santiago MATSim study tested 100 random seeds. Variation in individual link loads between seeds exceeded variation between the last two iterations within a seed. This is direct evidence that iteration stability and variation between independent runs answer different questions. [Paulsen et al., Procedia Computer Science, 2018](https://doi.org/10.1016/j.procs.2018.04.078).

Recommended practice is to repeat baseline and alternatives using a documented set of seeds and report the distribution of policy differences. Choose the number of runs for the precision the research question needs; there is no fixed replication count established by these sources. Paired seeds may help comparison when random streams remain comparable, but identical seed numbers do not guarantee that property after an intervention changes the simulation.

Report absolute changes as well as percentages. When the baseline is zero, percentage change is undefined. Test assumptions that could change the conclusion, such as demand, capacity, fares, value of time, fleet size or population sample fraction.

## Analyses selected for the research question

| Research question | Analyses to add | Additional data or model requirements |
| --- | --- | --- |
| Public transport changes | Ridership by line and stop, boardings/alightings, loads, waiting and in-vehicle time, transfers, access/egress time, delays and missed service | Transit schedule, stop/vehicle events, capacity and passenger associations |
| Demand-responsive transport or automated taxis | Served/rejected requests, waiting and detour distributions, occupancy, fleet utilization, empty versus occupied distance, service coverage | Request/service events, fleet schedules and service constraints |
| Pricing or infrastructure appraisal | Traveler utility changes, time/cost changes, revenue, operating/investment costs and external effects | Meaningful scores or a consistent utility model, monetary events and cost assumptions |
| Accessibility | Reachable jobs, schools or services by mode and time; changes by place or person group | Opportunity locations/counts and travel costs to potential destinations |
| Equity | Travel cost, time, accessibility and benefits by income, car availability, age or neighborhood; winners/losers and distribution | Relevant population attributes, consistent weights and explicit equity criteria |
| Climate and air quality | Vehicle travel, CO2 and pollutants by vehicle type, place and time; fleet scenarios | Vehicle technology, emission factors, road classification and emissions model |
| Noise | Exposure by time and location, affected residents and modeled damages | Noise model, receiver/population locations and traffic characteristics |

MATSim supplies a public-transit analysis command and DRT documentation covering service constraints and customer statistics. These are useful implementation references, but available outputs depend on the configured simulation. [PublicTransitAnalysis source](https://raw.githubusercontent.com/matsim-org/matsim-libs/main/contribs/application/src/main/java/org/matsim/application/analysis/pt/PublicTransitAnalysis.java), [DRT documentation](https://github.com/matsim-org/matsim-maas/blob/master/drt.md).

A Los Angeles MATSim research report examines vehicle miles traveled, transit use, empty automated-taxi travel, speed, greenhouse gases and accessibility benefits by income. It shows why fleet and distribution outcomes matter alongside passenger travel times. [Rodier, Kaddoura and Chai, UC Davis, 2022](https://ncst.ucdavis.edu/research-product/how-can-automated-vehicles-increase-access-marginalized-populations-and-reduce).

A Paris-Saclay MATSim application computes accessibility supplied by DRT feeding public transport. Accessibility evaluates opportunities people could reach, beyond their realized trips. The appropriate measure must therefore be specified, rather than inferred from average completed-trip duration. [Diepolder et al., 2023 author paper](https://arxiv.org/abs/2307.03148).

MATSim also includes analysis by sociodemographic groups and emissions aggregation by pollutant, vehicle category, link and grid. Emission totals require emission information, not ordinary movement events alone. Emissions are distinct from pollutant concentration and human exposure. [TripBySociodemographicGroupsAnalysis source](https://raw.githubusercontent.com/matsim-org/matsim-libs/main/contribs/application/src/main/java/org/matsim/application/analysis/population/TripBySociodemographicGroupsAnalysis.java), [AirPollutionAnalysis source](https://raw.githubusercontent.com/matsim-org/matsim-libs/main/contribs/application/src/main/java/org/matsim/application/analysis/emissions/AirPollutionAnalysis.java).

Economic appraisal requires consistent monetary valuation, costs and accounting boundaries. Toll revenue is a transfer between travelers and recipients; its treatment depends on the welfare boundary. Plan scores alone are insufficient to claim a complete cost-benefit result. A MATSim-related DRT appraisal framework discusses internal costs and monetized congestion, noise and air pollution. It is a methodological preparatory example, not evidence that every simulation directly supplies these quantities. [Grunicke et al., author manuscript](https://arxiv.org/html/2011.12869v2).

## Suggested analysis modules for this Rust repository

This order is an engineering recommendation based on the source review and the current checkout.

1. **Event audit and leg/journey reconstruction.** Export persons, legs, journeys and failures with stable external IDs, actual departure/arrival times, modes, activity purposes and completion status.
2. **Travel summaries.** Export mode shares, duration/distance distributions, departures by time/purpose and daily person totals.
3. **Network summaries.** Export link/time volumes and observed travel times, vehicle kilometres/hours and delay. Include vehicle entering/leaving traffic events when handling departure and arrival links.
4. **Validation and comparison.** Join observed counts, travel times and surveys; compare scenarios using consistent filters, units, weights and geography.
5. **Iteration and replication summaries.** Track meaningful outcomes over iterations and across seeds. Preserve a manifest containing configuration, input identity, seed, sample fraction and software version.
6. **Study-specific modules.** Add transit, DRT, accessibility, equity, emissions or appraisal when a study needs them and their input requirements are available.

Existing foundations:

- [EventsManager and typed events](../rust_qsim/src/simulation/events/mod.rs) support activity, person departure/arrival, link, vehicle and stuck-person events.
- [Event utilities](../rust_qsim/src/simulation/events/utils.rs) read single and partitioned XML/protobuf event files. Protobuf reading requires the associated ID store.
- [proto2xml](../rust_qsim/src/bin/proto2xml.rs) converts partitioned protobuf events to compressed XML for external processing.
- [Controller outputs](../rust_qsim/src/simulation/controller/controller.rs) include plans, network and the ID store. [Iteration event writer](../rust_qsim/src/simulation/controller/mod.rs) writes events per partition and configured iteration.

One current limitation affects the roadmap: `run_scoring_phase` assigns every plan the placeholder score `1.0`. Those scores cannot support behavioral score convergence, traveler welfare or consumer-surplus analysis. Start with experienced movement outcomes. Meaningful scoring is a prerequisite for score-based analyses. [Current scoring phase](../rust_qsim/src/simulation/controller/controller.rs).

The first deliverable should therefore be auditable person/leg/journey and link/time tables, followed by the core report. Java output filenames and available extensions should not be assumed to exist in this Rust implementation.

## Evidence and review limits

The official guide was read as a downloaded PDF. Java implementation sources were inspected from the mutable `main` branch; pin a version before copying algorithms. The NYC and Paris-Saclay papers were checked at abstract level, and the Los Angeles example at the university's report-summary level. Exact welfare, accessibility and emissions formulas need the full methods and input assumptions before implementation.

No universal error threshold, iteration count or seed count is recommended here. Establish those for the study's observations, intended decisions and required precision. This research changes documentation only and does not implement analysis modules.
