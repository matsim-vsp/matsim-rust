# Automatic analysis of the latest MATSim iteration

## Problem Statement

After running the Rust MATSim simulation, transportation researchers need usable analysis without manually merging event files, reconstructing travel or assembling charts. They need hourly speed, traffic volume and V/C for every road link, distributions across links, road coverage and classifications, and agent departure and travel-time patterns. They also need the validation, comparison and study-specific analyses identified in the research review.

Analysis must execute once after a successful simulation run and use only its latest completed iteration. Analyzing earlier iterations adds unwanted work and produces results outside the requested scope. Incomplete travel, unused roads and unavailable inputs must remain visible rather than biasing the results.

## Solution

When analysis is enabled, the simulation command automatically analyzes the latest iteration after all simulation outputs are finalized. It generates machine-readable tables, charts, coverage/congestion maps and a local HTML report before returning, then prints the report location.

The report includes core link, agent, journey, network and quality metrics. It includes additional research analyses when their prerequisites are configured and explicitly identifies unavailable modules. Researchers can regenerate a report from existing outputs without rerunning the simulation. Cross-run comparisons consume the latest-iteration results of each run.

"Ready when the run finishes" means ready when the command returns. Event replay and report generation have an execution cost after the last simulation timestep; no zero-latency guarantee is made.

## User Stories

1. As a transportation researcher, I want analysis to start automatically after my simulation finishes, so that I do not have to launch a separate reporting workflow.
2. As a transportation researcher, I want exactly one analysis of the latest completed iteration, so that earlier iterations do not add processing or enter my results.
3. As a transportation researcher, I want the command to print the completed report location, so that I can open the results immediately.
4. As a transportation researcher, I want a report that opens locally without a server or network connection, so that I can inspect and share it easily.
5. As a transportation researcher, I want exported tables alongside charts, so that I can reproduce figures and continue analysis in other tools.
6. As a transportation researcher, I want hourly speed and volume profiles for each directed road link, so that I can identify when and where traffic changes.
7. As a transportation researcher, I want entry and exit volumes in vehicles and passenger-car equivalents, so that I can distinguish traffic counts from capacity consumption.
8. As a transportation researcher, I want hourly V/C with its capacity and scaling assumptions, so that I can interpret traffic relative to the simulated network capacity.
9. As a transportation researcher, I want every eligible link included even when unused, so that low road coverage remains visible.
10. As a transportation researcher, I want used and unused link counts and percentages, so that I can recognize a case such as only 5 of 200 links carrying traffic.
11. As a transportation researcher, I want road-usage maps, so that I can see which portions of the network carry vehicles.
12. As a transportation researcher, I want inner-city and outer-city breakdowns, so that I can compare traffic conditions across urban areas.
13. As a transportation researcher, I want expressway and other link-type breakdowns, so that I can compare different road functions.
14. As a transportation researcher, I want lane-count and capacity-class breakdowns, so that I can compare roads of different sizes.
15. As a transportation researcher, I want missing road classifications shown explicitly, so that I know how much of the network lacks grouping data.
16. As a transportation researcher, I want one speed histogram per hour with speed bins on the X axis and link counts on the Y axis, so that I can examine the distribution across roads.
17. As a transportation researcher, I want comparable hourly V/C histograms, so that I can inspect how link utilization changes over the day.
18. As a transportation researcher, I want hourly mean and STD across links, so that I can describe variation in road performance.
19. As a transportation researcher, I want within-link traversal-speed mean and STD, so that I can distinguish variability among vehicles from variability among roads.
20. As a transportation researcher, I want unused links to have unavailable observed speed, so that zero traffic is not misrepresented as stopped traffic.
21. As a transportation researcher, I want departure counts grouped by hour and mode, so that I can identify travel-demand peaks.
22. As a transportation researcher, I want distinct departing-person counts alongside leg departures, so that repeated departures do not inflate the number of travelers.
23. As a transportation researcher, I want average leg travel time grouped by departure hour, so that I can compare departure cohorts.
24. As a transportation researcher, I want daily total and mean leg travel time per person, so that I can compare daily travel burdens.
25. As a transportation researcher, I want separate daily summaries for all complete persons and complete travelers, so that non-travelers and denominators remain explicit.
26. As a transportation researcher, I want incomplete legs and daily travel identified, so that missing arrivals do not reduce reported travel times artificially.
27. As a transportation researcher, I want journeys reconstructed across transit access and transfer legs, so that journey mode shares use the same unit as travel surveys.
28. As a transportation researcher, I want mode shares by purpose, hour, distance and area, so that I can understand travel behavior beyond aggregate totals.
29. As a transportation researcher, I want duration and distance distributions including medians and upper percentiles, so that averages do not conceal long trips.
30. As a transportation researcher, I want trips per person, mode chains and activity patterns, so that I can examine daily behavior.
31. As a transportation researcher, I want origin-destination summaries by mode and departure period, so that I can identify major travel flows.
32. As a transportation researcher, I want vehicle distance, vehicle time and free-flow-relative delay, so that I can quantify network performance.
33. As a transportation researcher, I want congestion maps and peak-period summaries, so that daily totals do not conceal localized problems.
34. As a transportation researcher, I want completed travel, stuck agents, unmatched events and warning summaries, so that I can assess whether the run is usable.
35. As a transportation researcher, I want comparisons with observed road counts and speeds, so that I can validate the simulated baseline.
36. As a transportation researcher, I want survey and transit-demand comparisons when observations exist, so that validation covers travel behavior and public transport.
37. As a transportation researcher, I want baseline-versus-alternative differences using consistent populations and geography, so that policy comparisons are interpretable.
38. As a transportation researcher, I want uncertainty summaries across repeated seeds, so that stochastic variation is visible in policy conclusions.
39. As a transportation researcher, I want sensitivity summaries across supplied parameter-varied runs, so that I can see which assumptions affect conclusions.
40. As a transportation researcher, I want transit ridership, loads, waits, transfers and delays when modeled, so that I can evaluate transit changes.
41. As a transportation researcher, I want served/rejected requests, detours, fleet utilization and empty travel for shared services, so that I can evaluate service quality and vehicle costs.
42. As a transportation researcher, I want accessibility to jobs, schools or services when opportunity and travel-cost data exist, so that I can evaluate potential access beyond realized trips.
43. As a transportation researcher, I want travel and policy outcomes disaggregated by available demographic groups, so that I can identify disparities and winners/losers.
44. As a transportation researcher, I want economic appraisal when meaningful utility and cost inputs exist, so that I can evaluate benefits, costs and transfers consistently.
45. As a transportation researcher, I want configured emissions and noise outputs by place and time, so that I can assess environmental consequences.
46. As a transportation researcher, I want runtime and measured resource information, so that I can understand the computational cost of simulation and analysis.
47. As a transportation researcher, I want unavailable and failed modules distinguished from genuine zero results, so that I do not interpret missing evidence as an outcome.
48. As a transportation researcher, I want to regenerate analysis with different bins or classifications, so that reporting changes do not require another simulation.
49. As a transportation researcher, I want failed analysis to preserve simulation outputs and support retries, so that reporting errors do not destroy the run.
50. As a transportation researcher, I want recorded definitions, input identities and settings, so that I can reproduce and audit every result.

## Implementation Decisions

- Add a shared analysis module with one high-level interface accepting run inputs, analysis settings and an output destination, and returning a summary or structured analysis error. Both automatic analysis and standalone reanalysis use it.
- Invoke analysis from the controller once after workers and external adapters stop, event writers close, final outputs are written and shutdown listeners complete. Do not invoke it from iteration callbacks.
- Select the configured final iteration of a successfully completed run. Standalone reanalysis uses recorded completed-run metadata. Do not select an iteration by the largest directory number or mix earlier iteration events into results. Record the iteration and expected partitions in the manifest.
- Add typed analysis configuration for enabled state, interval duration, fixed histogram edges, classification inputs, observed datasets and optional research analyses. Iteration selection is fixed to the latest completed iteration. Existing configurations opt in explicitly; reporting examples enable it.
- Validate required inputs and event recording before execution. Missing optional prerequisites yield an unavailable status and reason. Invalid explicitly required inputs fail analysis rather than silently reducing scope.
- Preserve compact person metadata before population mutation and expected-travel indicators immediately before the final mobsim. Preserve vehicle/PCE metadata and input identities for reanalysis. Maintain scenario ownership boundaries and avoid whole-scenario or population cloning.
- Replay all final-iteration event partitions chronologically using the existing event utilities, directly from supported XML/compressed XML or protobuf. Preserve the protobuf ID-store association. Do not replace the live global mapping during in-process analysis or create an intermediate merged XML file.
- Reconstruct traversals, legs and journeys in a single event pass. Stream detailed exports where practical and retain active-travel state and metric accumulators. Define simultaneous-event handling explicitly and prevent partition tie order from deciding outcomes.
- Use directed eligible road links as the network denominator. Keep urban area, road type and road size independent. Require supplied classification rules and retain an unknown group. Hourly intervals include their start and exclude their end; retain simulated hours beyond 24.
- Export a row for every link/hour. Count entries/exits once, including correctly handled departure and arrival links. Support repeated vehicle visits and distinguish complete from partial traversals.
- Assign full-link speed observations to entry hour. Representative speed is total observed distance divided by total traversal time. Export arithmetic mean and population STD of individual traversal speeds separately. Reject invalid/non-positive traversal durations with quality counts.
- Across-link speed summaries and histograms give each valid representative link speed one contribution. Unused links have zero volume and unavailable observed speed. Report unused, observed and invalid counts alongside every distribution.
- Compute V/C from PCE flow and effective interval capacity, including sample-size scaling. Do not multiply already defined link capacity by lanes again. Export entry and exit V/C separately; use labeled entry V/C in the default histogram. Missing/non-positive capacity or required PCE makes the ratio unavailable.
- Keep histogram edges fixed across hours and comparable runs, with documented boundary conventions and an overflow bin. Population STD is zero for one observation and unavailable for an empty group.
- Reconstruct actual leg durations from person departure/arrival events and group them by departure hour. Report both leg departures and distinct persons. Daily travel time sums leg durations; exclude activity time. Retain incomplete-person partial totals without presenting them as complete-day totals.
- Compute daily means with explicit complete-person and traveler denominators. Include verified non-travelers as zero only in the complete all-person daily total mean. Do not infer non-travel status from absent events alone.
- Reconstruct journeys between substantive activities using declared stage-activity and main-mode rules. Export separate journey and leg statistics, purpose/mode/distance distributions, daily patterns and zoned OD flows. Distinguish observed, route, straight-line and model-derived distances.
- Add network distance/time and free-flow-relative delay totals, coverage and congestion maps. Distinguish vehicle from passenger totals, state occupancy prerequisites, and declare partial-traversal and cross-hour allocation conventions.
- Produce validation comparisons for supplied counts, speeds/travel times, surveys and transit demand. Match geography, periods, classes and units; document expansion and calibration versus held-out data. Report denominators, bias, MAE/RMSE and count-specific GEH where appropriate. Zero reference values make relative errors unavailable.
- Compare latest-iteration reports across runs using explicit baseline, consistent filters and geographic/link correspondence. Report absolute changes, defined percentage changes, failures and exclusions. Aggregate supplied repeated seeds and sensitivity scenarios; disclose interval methods and comparability of random streams.
- Conditional transit/shared-service analyses require schedule, capacity, request and passenger/service records. Accessibility requires opportunity data and potential-destination costs. Equity requires demographic data and stated criteria. Appraisal requires meaningful utilities, costs and a transfer/accounting boundary. Environmental outputs require configured emissions/noise models and exposure inputs. Support applicable analyses and explicit prerequisite/status reporting; do not infer missing model outputs from ordinary movement events.
- Current placeholder plan scores are unsuitable for welfare, consumer surplus or behavioral convergence. Mark such metrics unavailable until meaningful scoring or an external utility model supplies their prerequisites.
- Write CSV/JSON tables, a manifest, module status and a self-contained HTML report with embedded charts and maps. Local viewing must require neither a server nor network downloads. Prefer existing serialization dependencies.
- Generate reports in a separate staging destination and publish completion only after required outputs succeed. Keep simulation outputs intact on errors and retries. Preserve established controller API/error behavior unless a deliberate API change is documented. Never announce complete analysis after required-module failure.
- Record run/scenario and iteration identity, seed, input/software identity, units, bins, grouping, scaling, filters and exclusions. Use stable export order and deterministic aggregation. Record computation timing separately from transportation outcomes.
- Deliver reconstruction/quality and requested link/agent outputs first, then journey/network summaries, validation/comparison/uncertainty and conditional research modules. The specification retains the full scope; missing optional data must remain explicit throughout delivery.

## Testing Decisions

- Prefer one shared analysis interface as the primary test seam. Tests supply a small complete run bundle and assert exported values, report status and artifacts. Do not assert private accumulator layout, callback counts or internal dispatch behavior.
- Add a real simulation-command integration test through the existing controller execution seam. Use multiple iterations with deliberately different outcomes and verify one complete report containing only the final iteration's values. Earlier iteration reports must be absent. This is an external behavior assertion, not a mock invocation-count test.
- The user approved the ticket breakdown, which includes this shared-interface test seam and final-only simulation-command verification.
- Use existing controller iteration/output tests as prior art for lifecycle integration, existing single/partitioned event-reader and conversion tests for input formats, and existing deterministic tests for ID-store isolation.
- Cover hand-computable volume, speed, harmonic/arithmetic averaging, STD, histogram boundaries, V/C/PCE/sample scaling, road groups and the 5-used-of-200-links coverage example.
- Cover cross-hour and post-midnight travel, repeated link visits, same-link departure/arrival, partial traversals, transfers, distinct-person versus departure counts, non-travelers, stuck persons, invalid durations and incomplete daily travel.
- Compare automatic and standalone analysis on the same bundle. Compare XML/protobuf and single/multiple-partition results using stable identities and tolerance-based numerical assertions where appropriate. Verify that harmless simultaneous-event order changes do not alter metrics.
- Check missing final events/partitions, inconsistent IDs, unavailable classifications, invalid capacities, missing conditional prerequisites and required-module failures. Verify that missing data do not turn into zero metrics.
- Verify local HTML/table artifacts, report iteration identity, module status, finalization, reruns and interrupted staging outputs. Failed reporting must leave raw simulation outputs available and no completed-report claim.
- Test validation/comparison with known observations and baseline differences, including zero reference values, changed networks and completion-population exclusions. Test conditional analyses against small explicit fixtures for their supplied prerequisites.
- Tests using IDs or logger setup must use the repository's deterministic-ID macro and appropriate integration-test exclusivity. Start with focused checks, then run formatting and proportionate workspace validation during implementation.

## Out of Scope

- Analysis of earlier iterations, per-iteration reports and convergence/iteration-trend analysis.
- Live dashboards or per-event cross-worker shared analysis locks. Online collection is a later optimization only if measured replay cost warrants it.
- Automatically launching seed ensembles, parameter sweeps or policy simulations. Comparison modules consume supplied completed runs.
- Enabling or changing scenario behavior, replanning, scoring, routing or capacity dynamics to make analysis possible.
- Building new transit, DRT, emissions, noise, dispersion or economic simulation engines; analysis consumes their available outputs and records missing prerequisites.
- Synthesizing observed counts, demographic attributes, opportunities, road classifications, zone boundaries or missing observations.
- Universal validation thresholds, guaranteed convergence or zero additional reporting latency.
- Hosted dashboards, deployment, new tracker configuration or changes to repository permissions/settings.

## Further Notes

The selected scope combines the [analysis requirements](simulation-analysis-requirements.md) and [source review](matsim-result-analysis-research.md). The research is targeted, not systematic. Its iteration-stability findings remain background evidence and are excluded from implementation by the user's latest-iteration-only instruction.

No implementation changes have been made. The user approved the 21-ticket breakdown. The tickets are published to GitHub with `ready-for-agent` and native blocking relationships; see the [published ticket index](automatic-result-analysis-tickets.md). This spec remains a local source document, and no parent issue was created or modified.
