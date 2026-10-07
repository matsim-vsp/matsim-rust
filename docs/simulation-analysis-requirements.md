# Simulation result analysis requirements

This specification combines the user's requested link and agent analyses with the findings in [Analyses to run after MATSim](matsim-result-analysis-research.md). It defines proposed defaults for implementation; it does not implement the analysis.

## Analysis coverage

The core report applies to every run. Additional analyses belong in the same reporting framework when their inputs and corresponding simulation models are available. Missing prerequisites must produce an explicit unavailable status and reason, rather than fabricated values.

| Analysis | Coverage | Main outputs |
| --- | --- | --- |
| Run integrity | Core | Completed/failed travel, event quality, warnings, missing partitions |
| Link performance and road coverage | Core | Hourly speed, volume, V/C, AVG/STD, histograms and used/unused links |
| Agent and journey behavior | Core | Departures, travel times, mode shares, purposes, distances and origin-destination flows |
| Congestion and network totals | Core | Delay, vehicle distance/time and spatial hotspots |
| Empirical validation | When observed data exist | Observed-versus-simulated counts, speeds, survey travel and transit demand |
| Scenario comparison | When baseline and alternatives exist | Absolute/relative changes, maps and affected groups |
| Uncertainty and sensitivity | When repeated/parameter-varied runs exist | Variation across seeds, policy-difference intervals and sensitivity |
| Transit and shared services | When modeled | Ridership, waiting, loads, transfers, service failures and fleet performance |
| Accessibility and equity | With opportunity and demographic data | Reachable opportunities and distribution of outcomes |
| Economic appraisal | With meaningful utility and cost inputs | User benefits, revenues, costs and welfare accounting |
| Emissions and noise | With environmental models and inputs | Pollutants, greenhouse gases, noise exposure and spatial effects |
| Computational performance | With timing/resource records | Runtime, phase timing and resource use |

These categories cover the reviewed research and the user's selected scope. They are not a claim that ordinary QSim output supports every possible transport research question.

## Scope and grouping

Run analysis exactly once after a simulation successfully finishes, using only its latest completed iteration. For a normal completed run, this is the controller's configured `last_iteration`. Read all event partitions for that iteration and record its number in the report manifest. Earlier iteration outputs are outside the analysis input. Use one-hour intervals, with configurable bin duration. Intervals include their start and exclude their end. Retain hours beyond 24 when the simulated day extends past midnight.

Use directed network links as the reporting unit. Two directions of a physical road count as two links unless an explicit road aggregation is supplied. Establish the eligible road network before measuring usage, so non-road links do not inflate the denominator.

Keep these grouping dimensions independent:

| Dimension | Examples | Required input |
| --- | --- | --- |
| Urban area | Inner city, outer city, unknown | Explicit link labels or geographic boundaries and a documented assignment rule |
| Link type | Expressway, arterial, collector, local road, unknown | Existing road-type attributes or an external link-ID classification |
| Road size | Lane count, capacity class | Network lanes and capacity, with documented class boundaries |

An expressway may lie in the inner or outer city. Do not infer road type from speed alone. Preserve missing classifications as `unknown` and report their counts. For links crossing area boundaries, record the geographic assignment rule.

## Hourly link data

Export a row for every eligible link and hour, including unused links.

| Field | Definition |
| --- | --- |
| `link_id`, `hour_start`, `hour_end` | Stable external link ID and interval |
| `urban_area`, `link_type`, `lanes`, `length_m` | Link classification and network geometry |
| `entry_volume_vehicles`, `exit_volume_vehicles` | Number of vehicles entering and leaving during the interval |
| `entry_volume_pce`, `exit_volume_pce` | Same flows weighted by passenger-car equivalents |
| `capacity_pce_per_hour` | Network flow capacity |
| `effective_capacity_pce_per_bin` | Capacity adjusted to the simulation's scale and interval |
| `entry_vc`, `exit_vc` | Entry or exit PCE flow divided by effective interval capacity |
| `speed_observation_count` | Number of valid complete-link traversals used for speed |
| `speed_kmh` | Total observed full-link distance divided by total traversal time, converted to km/h |
| `vehicle_speed_mean_kmh`, `vehicle_speed_std_kmh` | Arithmetic mean and population standard deviation of individual traversal speeds |
| `freeflow_speed_kmh` | Network free-flow speed, labeled separately from observed speed |
| `unfinished_traversals`, `invalid_observations` | Observation quality counts |

Assign complete-link speed observations to the hour of entry, even when the vehicle leaves in a later hour. Entry and exit volumes use their respective event times. This means volume and speed sample counts may differ.

For a complete traversal, speed is `3.6 * length_m / traversal_seconds`. The representative link speed uses total distance divided by total time. On a fixed-length link this equals the harmonic mean of individual traversal speeds. Store the arithmetic mean separately to make the averaging convention explicit.

Match traversals using vehicle ID and link ID, allowing repeated visits. Vehicle-entering/leaving-traffic events need explicit treatment for departure and arrival links. Count each entry or exit once, without double-counting overlapping event types. Exclude partial-link traversals from full-link speed statistics unless the traveled distance is known; retain them in traffic-volume accounting.

Reject non-finite values and non-positive traversal durations from speed calculations and report them. Event-derived speed includes time spent traversing and queuing on the link; it is not instantaneous vehicle speed. Teleported travel supplies agent travel times but no observed road-link speed.

### Capacity and V/C

The local simulator scales hourly capacity by `qsim.sample_size` and consumes flow capacity using vehicle PCE. For a constant-capacity link and interval duration `bin_seconds`, use:

```text
effective_capacity_pce_per_bin = capacity_pce_per_hour * sample_size * bin_seconds / 3600
entry_vc = entry_volume_pce / effective_capacity_pce_per_bin
exit_vc = exit_volume_pce / effective_capacity_pce_per_bin
```

These definitions follow the current [flow-capacity calculation](../rust_qsim/src/simulation/network/flow_cap.rs) and [PCE consumption](../rust_qsim/src/simulation/network/link.rs). Verify any capacity modifications when implementing the analysis. Do not multiply link capacity by lane count again.

Use entry V/C for the default histogram and label it explicitly. Exit V/C reports discharged flow. Neither directly measures demand waiting outside the link; do not interpret a low V/C as proof that no congestion occurred. Show observed speed alongside it.

With missing or non-positive capacity, V/C is unavailable. Missing PCE also makes the affected ratio unavailable unless an explicit assumption is configured. Preserve raw simulation counts; any expanded population volumes must be separate, consistently labeled fields.

## Coverage and hourly graphs

Report total eligible links, used links, unused links, links with observed speed, and usage percentage for each hour and for the entire simulated day. A daily used link has at least one entry anywhere in the day, rather than the sum of hourly used-link counts.

For the example of 200 eligible links and 5 used links, display 5 used, 195 unused and 2.5% usage. Also show which links are used on a map and break coverage down by area and link type.

Produce these plots:

| Plot | X axis | Y axis | Grouping |
| --- | --- | --- | --- |
| Link speed over time | Hour | Observed km/h | Selected link, or area/type summary |
| Traffic volume over time | Hour | Vehicles per hour or PCE per hour | Selected link, or area/type summary |
| Speed histogram for each hour | Fixed speed bins in km/h | Number of links | All eligible links with observed speed, then area/type filters |
| V/C histogram for each hour | Fixed entry V/C bins | Number of links | All links with valid capacity, then area/type filters |
| Coverage over time | Hour | Used and unused link counts | All links, area, type and road-size class |

Use the same histogram edges across hours and runs so plots remain comparable. Configure edges and retain an overflow bin. Show unused and unavailable counts beside the graphs.

Unused links have zero traffic volume and zero V/C when capacity is valid. Their observed speed is unavailable, not zero or free-flow speed. V/C plots should distinguish unused links from used links within their lowest ratio bin.

For each hour and road group, calculate the arithmetic mean and population standard deviation across valid representative link speeds. Each link contributes once to this summary and the speed histogram. This describes variation between links. The per-link `vehicle_speed_std_kmh` describes variation between traversals. Empty groups have unavailable mean and STD; a single observation has population STD zero.

Group-level average V/C can also be reported, but distinguish the arithmetic mean of individual ratios from total group flow divided by total group capacity. Use separate fields if both are exported.

## Agent travel patterns

Reconstruct actual legs from person departure and arrival events. Export person ID, leg sequence, mode, departure time, arrival time, duration and completion status.

| Output | Definition |
| --- | --- |
| Departure profile | Number of leg departures in each hour, overall and by mode |
| Departing-agent profile | Number of distinct persons departing in each hour |
| Travel time by departure cohort | Arithmetic mean completed-leg duration for legs departing in the interval, with completion/failure counts |
| Daily person data | Completed-leg count, sum of completed-leg travel times, mean completed-leg travel time and daily completion status |
| Population daily summary | Mean daily total travel time across persons with complete observed travel days; also report the mean among travelers |

Use the leg's departure hour when grouping travel time, even if arrival occurs in a later hour. A person may depart multiple times in one hour, so leg count and distinct-person count differ.

For complete persons, daily travel time is the sum of leg durations, not the time between the day's first departure and last arrival. Include verified non-traveling persons as zero in the all-person daily mean. For incomplete persons, report observed partial totals and unavailable complete-day totals; do not treat a missing arrival as zero travel time.

The population mean per leg weights each completed leg equally. The population mean daily total weights each complete person equally. Export both, with denominators and exclusions. If daily person totals are grouped by departure cohort, use first daily departure and keep non-travelers in a separate group.

### Journeys, mode choice and activity patterns

Keep the requested per-leg outputs and also reconstruct journeys between substantive activities. Combine transit access, transit and egress legs into a journey, with a documented rule for stage activities and transfer waiting. Report leg duration and journey duration in separate columns.

Export journey origin/destination, departure/arrival, purpose, analysis main mode, component modes, distance, total elapsed travel time and completion status. Calculate:

- Journey mode shares overall and by hour, purpose, distance class, urban area and available demographic group.
- Duration and distance distributions with count, mean, STD, median and upper percentiles.
- Journeys per person, daily mode chains, and activity start/end times and durations.
- Origin-destination matrices by mode and departure period, with a documented zone system and boundary-crossing rule.

Define mode-share denominators explicitly. A walk-transit-walk journey contributes once to transit journey share; its individual legs remain available in leg summaries. State whether distance is route distance, observed traveled distance or straight-line distance. Teleported distance must retain its model-derived status.

The journey outputs extend the official [TripAnalysis](https://raw.githubusercontent.com/matsim-org/matsim-libs/main/contribs/application/src/main/java/org/matsim/application/analysis/population/TripAnalysis.java) and [analysis-main-mode definitions](https://www.matsim.org/doxygen/interfaceorg_1_1matsim_1_1core_1_1router_1_1_analysis_main_mode_identifier.html).

## Congestion and network totals

Supplement the link histograms with vehicle kilometres traveled, vehicle hours traveled, and delay relative to free flow, overall and by hour, area, type and mode.

For a complete traversal, reference free-flow time is `length_m / freespeed_m_per_second`. Report signed observed-minus-reference time; if a non-negative excess-delay measure is also exported, label its clipping rule. Invalid reference speed makes delay unavailable. Use known traveled distances for partial-link records and do not count a full link twice for a same-link departure/arrival.

Vehicle distance/time and passenger distance/time are different quantities. Passenger totals require occupancy or passenger association. Link traversal totals must disclose exclusions of incomplete observations. Time-based allocation of distance/time across hours requires an explicit allocation convention; ordinary enter/leave events do not reveal a vehicle's detailed motion within a link.

Produce congestion maps, link speed relative to free flow, delay distributions and peak-period summaries. An en-route-agent profile can help distinguish a short rush-hour peak from travel that remains unfinished at the simulation end. The research references for these outputs include [TrafficAnalysis](https://raw.githubusercontent.com/matsim-org/matsim-libs/main/contribs/application/src/main/java/org/matsim/application/analysis/traffic/TrafficAnalysis.java) and the [MATSim User Guide](https://matsim.org/files/book/partOne-latest.pdf).

## Run integrity

Publish a quality summary before interpreting outcomes:

- Input persons and planned travel, observed departures/arrivals, completed legs/journeys, unfinished travel, stuck persons and verified non-travelers.
- Failure counts by hour, mode and location, including incomplete travel at simulation end.
- Unmatched/duplicate events, invalid durations, missing classifications, unknown IDs and missing input or event partitions.
- Warning/error summaries and the inclusion/exclusion rules used for each metric.

Distinguish persons from simulated drivers and service vehicles where the model creates additional agents. Missing activity in an event file alone does not prove a person stayed home. Read the population and reconcile its expected travel with the observed records.

Compute this quality report from the latest iteration and its expected-travel metadata. Iteration-stability analysis from the research review is excluded from the selected scope because it requires earlier iterations. A latest-iteration report alone cannot establish convergence. [Official guide](https://matsim.org/files/book/partOne-latest.pdf), [maintainer discussion of unfinished travel](https://github.com/matsim-org/matsim-libs/issues/3008).

## Empirical validation

Compare the baseline with external observations before using it for policy claims. Support the following datasets independently, so partial validation remains visible:

| Observations | Comparison outputs |
| --- | --- |
| Road counts | Hourly/daily link or screenline counts, scatterplots, residual maps and error tables |
| Speeds or travel times | Matched link/corridor and time-period comparisons, bias and error distributions |
| Travel surveys | Journey mode shares, purpose, distance/duration distributions and departure profiles |
| Transit observations | Stop/station boardings, line loads and time profiles |

Match observation and simulation geography, dates, units, vehicle classes and time periods. Record sample expansion and observation uncertainty. Keep calibration observations distinct from held-out validation when possible.

Report signed bias, MAE and RMSE where appropriate. GEH is an additional traffic-count diagnostic and needs consistent count periods. Relative errors are unavailable when the observation is zero; retain absolute errors. Each score must show sample size and missing matches. Acceptance criteria belong to the study and its evidence, rather than one universal threshold.

The [NYC study](https://arxiv.org/abs/2008.04762) demonstrates speed, road-count and transit validation. The official [CountComparisonAnalysis](https://raw.githubusercontent.com/matsim-org/matsim-libs/main/contribs/application/src/main/java/org/matsim/application/analysis/traffic/CountComparisonAnalysis.java) is an implementation reference.

## Scenario comparison, uncertainty and sensitivity

Each individual run produces the tables above. A separate comparison reads those outputs for a baseline and alternatives; it must not mix events from different runs.

Compare the user's hourly link metrics, coverage, histograms, agent departure cohorts and daily travel, together with all enabled research metrics. Export baseline value, alternative value, absolute difference, percentage difference where defined, and denominator. Add difference maps and breakdowns by area, road type, mode and population group.

Use a consistent analysis population, geography, weighting and completion rule. Report intervention-induced failures separately. For metrics excluding failed travelers, disclose whether the comparison uses persons completing both scenarios and how many were excluded. If networks change, use explicit link correspondence or compare stable corridors/zones and network totals.

For repeated seeds, export the number of runs, mean, STD, quantiles and the distribution of scenario differences. Confidence intervals, when used, must state their method and assumptions. Distinguish variation between travelers, variation over iterations and variation between independent runs. Choose replication counts for the required precision; matching seed numbers alone does not guarantee comparable random streams.

Sensitivity runs should vary assumptions relevant to the claim, such as demand, capacity, sample fraction, behavioral coefficients, fares or fleet size. Report whether the policy conclusion changes. The [Santiago seed-variability study](https://doi.org/10.1016/j.procs.2018.04.078) and [NYC policy evaluation](https://arxiv.org/abs/2008.04762) provide evidence for these requirements.

## Transit and shared-service analyses

| Module | Required outputs | Prerequisites |
| --- | --- | --- |
| Public transport | Stop/line ridership, boardings/alightings, occupancy/load factors, access/egress, waiting, in-vehicle time, transfers, service delays and missed service | Schedule, vehicle capacities, service and passenger events |
| Demand-responsive transport and taxis | Served/rejected requests, waits and detours, occupancy, fleet utilization, empty/occupied distance and service-area coverage | Request events, fleet schedules, passenger associations and service constraints |

Separate passenger waiting from vehicle idle time. Request rejection needs request records; it cannot be inferred just from completed passenger trips. Publish capacity and maximum-wait constraints alongside service results. Teleported public transport cannot supply congestion-based physical transit-vehicle performance without additional modeling.

Use the official [PublicTransitAnalysis](https://raw.githubusercontent.com/matsim-org/matsim-libs/main/contribs/application/src/main/java/org/matsim/application/analysis/pt/PublicTransitAnalysis.java) and [DRT documentation](https://github.com/matsim-org/matsim-maas/blob/master/drt.md) as references. The [Los Angeles report](https://ncst.ucdavis.edu/research-product/how-can-automated-vehicles-increase-access-marginalized-populations-and-reduce) illustrates the importance of empty vehicle travel and transit substitution.

## Accessibility, equity and economic appraisal

Accessibility should measure opportunities reachable by mode and departure period, using a declared method such as opportunities within a travel-time threshold or a cost-weighted measure. It requires opportunity data and travel costs to potential destinations, including destinations not visited in the realized plan. Completed-trip averages alone cannot produce this result. Export accessibility by person or zone, maps and scenario differences. [Paris-Saclay shared-mobility accessibility study](https://arxiv.org/abs/2307.03148).

Equity summaries should disaggregate travel time, monetary cost, accessibility and policy changes using available income, age, car-availability or neighborhood attributes. Report group sizes, weights, distributions and shares gaining/losing. Missing demographic attributes remain explicit. Group averages describe disparities; any stronger equity judgment requires a stated criterion.

Economic appraisal should state the utility model, monetary conversion, included population and accounting boundary. Export traveler benefits, fares/tolls, operator revenue, operating/investment costs and modeled external effects. Treat transfers consistently and prevent double counting. The [NYC study](https://arxiv.org/abs/2008.04762), [Los Angeles report](https://ncst.ucdavis.edu/research-product/how-can-automated-vehicles-increase-access-marginalized-populations-and-reduce), and [DRT appraisal framework](https://arxiv.org/html/2011.12869v2) motivate these additions.

The current Rust [scoring phase](../rust_qsim/src/simulation/controller/controller.rs) assigns placeholder scores of `1.0`. Until a meaningful scoring or external utility model exists, score-based convergence, consumer surplus and welfare must be marked unavailable. Observed movement outcomes and demographic breakdowns can still be reported.

## Environmental and computational analyses

Emissions outputs should cover configured pollutants and greenhouse gases by vehicle category, link/area and time. Record fleet technology, emission-factor source, warm/cold-start treatment, sample expansion and accounting boundary. Noise outputs should identify modeled sound metrics, receivers, exposure periods and affected population. Movement events alone do not provide these results; emission concentration, human exposure and monetized damage require further models.

Produce totals, maps, time profiles and baseline differences when those models are enabled. The [AirPollutionAnalysis implementation](https://raw.githubusercontent.com/matsim-org/matsim-libs/main/contribs/application/src/main/java/org/matsim/application/analysis/emissions/AirPollutionAnalysis.java), [Los Angeles report](https://ncst.ucdavis.edu/research-product/how-can-automated-vehicles-increase-access-marginalized-populations-and-reduce), and [appraisal framework](https://arxiv.org/html/2011.12869v2) document environmental analysis examples and requirements.

Record total runtime and phase timing for simulation, replanning, input/output and analysis when available. Include worker count, hardware, build configuration and workload size for computational comparisons. Report peak memory only if actually measured. Computational performance describes execution cost and does not establish transportation-model validity. The official guide describes iteration stopwatch outputs.

## Deliverables and remaining inputs

The core report consists of link metadata, hourly link metrics, hourly group summaries, histogram-bin counts, individual legs/journeys, daily person totals, departure-cohort summaries, mode/purpose/OD summaries, network totals and a quality report. Export machine-readable tables alongside graphs and coverage/congestion maps.

Add separate tables for validation, scenario differences, replication/sensitivity, transit/shared services, accessibility/equity, appraisal, environmental effects and computation when applicable. Comparisons and replication summaries consume each run's latest-iteration report. A report index must show each module's status, available metrics, prerequisites and exclusions. Distinguish a genuine zero result from unavailable data.

Retain a run manifest with run/scenario identity, iteration, seed, input identity, software version, configuration, time-bin and histogram settings, sample scaling, grouping rules, included modes, population filters and units. Read every event partition with its associated network, vehicle data, population and ID mapping. Establish stable export ordering and consistent treatment of simultaneous events so partition count or input format does not create arbitrary differences.

Implement in this order: reconstruction and quality checks; the requested link/agent outputs; journey/network summaries; validation; comparisons and uncertainty; then the conditional research modules. This order provides useful core results while making additional data and model prerequisites explicit.

Before implementation, identify the actual link-type attributes and inner/outer-city labels or boundary data in the intended scenarios. Histogram edges and road-size classes can use documented configurable defaults. No simulation outputs or classification datasets were supplied for analysis in this request.

## Automatic execution after a run

The proposed user experience is one simulation command that produces simulation outputs and the analysis report. With analysis enabled, the command reports completion after the analysis phase finishes and prints the report path. There is an analysis cost after the last simulated timestep; the design does not promise zero additional latency.

### Controller integration and shared interface

Add `simulation::analysis` as a module that owns event reconstruction, metrics, input validation and report writing. Its public interface accepts the run inputs, analysis configuration and output destination, and returns an analysis summary or an error. Keep metric implementations behind that interface.

Call it inside [Controller::run](../rust_qsim/src/simulation/controller/controller.rs) after workers/adapters have stopped, final output files have been written, and shutdown listeners have completed. This location also covers the routing entry point, because both simulation binaries call the controller. The current shutdown path closes event writers when the worker loop exits. `AfterMobsim` and `IterationEnds` do not currently guarantee closed event files, so they are unsuitable replay triggers without lifecycle changes.

Expose the same module through a proposed `analyze_results` binary for rebuilding reports from saved outputs. Analysis settings and report changes should not require rerunning the simulation. A separate comparison command can consume reports from multiple runs.

### Input preservation and one replay

Before the first iteration, preserve the compact input-person metadata needed to distinguish planned travel, failures and non-travelers. Store IDs, classification/demographic fields and planned-travel indicators; do not clone the entire scenario or population. Keep metadata associated with the input ID mapping and run manifest. Where an original input bundle is retained, record its identity and location for standalone reconstruction.

Immediately before the final iteration's mobsim, record its expected travel indicators, since earlier replanning may change them. Preserve vehicle identifiers and PCE/type metadata as well; final output plans alone do not provide all inputs required for repeatable link-capacity analysis.

Read the final iteration's event partitions directly using [read_partitioned_events](../rust_qsim/src/simulation/events/utils.rs). XML and protobuf inputs already have chronological partition readers. There is no need to create a merged XML copy first. Protobuf replay uses the matching ID store; in-process analysis must not replace the live global mapping.

Use one chronological event pass to reconstruct legs, journeys and vehicle/link traversals and feed the core metrics. Stream detailed records to output and retain only active-travel state and aggregation data where practical. Memory requirements still depend on simultaneous travelers, histogram detail, OD groups and the selected outputs. Define handling of simultaneous events explicitly; the existing reader's partition tie order is not evidence of causality.

Invoke analysis once after the complete controller shutdown sequence, rather than from an iteration callback. Select the final iteration from the completed-run metadata, not by scanning for the highest numbered directory, which might contain incomplete outputs. Validate that final-iteration event recording is enabled before starting an automatic replay-based run.

### Configuration and report files

Add a typed `Analysis` configuration module using the existing config mechanism. Proposed settings include enabled state, interval duration, speed/V/C histogram edges, classification/boundary inputs, observed datasets and optional study modules. Iteration selection is fixed to the latest completed iteration. Use an explicit opt-in default for existing configurations and enable analysis in new example configurations intended to generate reports.

Write the following beneath the run output:

```text
analysis/
  index.html
  manifest.json
  status.json
  tables/
  plots/
```

Generate a self-contained HTML report with embedded SVG charts and simple filters, plus CSV/JSON tables. It should open from a local file without a server, external chart downloads or network access. Use the existing CSV and JSON dependencies initially. Keep environmental, accessibility, demographic and observed datasets configurable rather than required for the core report.

### Completion, errors and retries

Validate configured required inputs early. After simulation completion, a failed required analysis must produce an explicit failed status, diagnostic and incomplete-report message. Under the controller's current unit-returning interface, this can use its established failure convention; any future structured result should be an explicit API change. Missing optional prerequisites should be recorded as unavailable and allow the core report to complete.

Write each report into a separate staging directory. Publish its completion marker and final index only after required outputs succeed. Retrying analysis regenerates only analysis artifacts and preserves simulation files; incomplete attempts must never appear as completed reports. Module status must distinguish completed, unavailable and failed.

### Later performance optimization and verification

Measure replay and rendering cost before adding online aggregation. If replay becomes a bottleneck, collect suitable link/time statistics in worker-local event handlers, consolidate them in stable order after workers finish, and reuse the same report calculations. Do not add a shared lock to every event. Person journeys crossing partitions need explicit ownership or transferred state, so worker-local summaries alone cannot reconstruct them safely.

Implementation verification should cover known link/agent metrics, unused links, incomplete travel, same-link routes, cross-hour journeys, PCE/sample scaling, simultaneous events, multiple partitions, XML/protobuf equivalence, report retries and errors. Drive the simulation command on a small scenario and inspect its actual report and exported values before claiming automatic reporting works. This execution design is proposed; no controller changes or analysis binary exist yet.
