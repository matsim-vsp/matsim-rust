# Automatic analysis

Automatic final-iteration analysis can be enabled with `output.analysis.enabled`.
It writes an offline HTML report and CSV/JSON/SVG files under `output/analysis`.

## Economic appraisal

Set `output.analysis.economic_inputs` to a CSV to export explicitly supplied welfare and cost
records. The input is read once for the latest completed iteration and recorded in the manifest
for standalone reanalysis. No plan score is used as welfare evidence.

```csv
scope,entity_id,group,account,value,unit,marginal_utility_of_money,money_unit,transfer_id,source
person,p1,workers,utility,10,utils,2,USD,,model
person,p1,workers,fare,3,USD,,USD,fare-1,ticketing
run,,,operator_operating_cost,100,USD,,USD,,accounts
run,,,operator_investment_cost,50,USD,,USD,,capital-plan
run,,,external_cost,20,USD,,USD,,valuation
```

`scope` is `person`, `group`, or `run`; person and group rows require `entity_id`, and run rows
leave it blank. Supported accounts are `utility`, `fare`, `toll`, `operator_revenue`,
`operator_operating_cost`, `operator_investment_cost`, and `external_cost`. Utility conversion is
`value / marginal_utility_of_money`; its money unit is required. A missing or invalid conversion
leaves utility in utils and marks its money equivalent unavailable. Fare and toll amounts create
matching traveler-payment and operator-revenue rows with the same `transfer_id`. Both transfer
rows are excluded from net social accounting. A separately supplied `operator_revenue` also needs
a `transfer_id` and is excluded. Do not repeat fare or toll amounts as operator revenue. Operating,
investment, and external costs remain separate. The summary lists accounts that were not supplied
or could not be converted; omitted costs are not estimated.

`economic_appraisal.csv` is the row-level ledger and `economic_summary.csv` groups its converted
monetary values by entity and account. `total` preserves supplied positive cost amounts;
`net_social_value` subtracts operating, investment, and external costs and leaves transfers blank.
Every group and run scope also gets a `net_social_value` account row per money unit. It is
`available` only when that scope supplied utility and all three cost categories in that same
currency; a scope never borrows another scope's numbers, and person utility never stands in for a
group total. A scope's missing accounts are marked `unavailable_not_supplied_at_scope` and its
incomplete net is `unavailable_missing_inputs`, so an unavailable net is never read as zero. A run
row is emitted for every unit seen anywhere, including when only person or group records supplied
it. Person utilities remain comparable per person, without allocating group/run costs to
individuals. Sums that stop being finite are marked `unavailable_overflow` rather than reported as
a number.
The ledger keeps the supplied marginal utility of money beside both the utility and converted rows.
Repeated records are summed by their person/group/run, account, and money unit in the comparison.
The summary marks each account included in net social accounting. Do not add person, group, and run
rows together because they may describe the same costs at different aggregation levels. The
cross-run comparison reports alternative minus baseline from each run's latest completed report.
Person rows use people with complete travel records in both runs; group and run rows retain the
supplied denominators. Person economic rows outside that shared complete-person cohort remain in
`metric_differences.csv` with `excluded_incomplete_or_missing_travel` status and no difference.
Monetary units are part of comparison keys, so different currencies do not compare as if they
shared a unit.
## Execution context

The local report includes `runtime.csv` and `runtime_metadata.json`, separate from the deterministic
simulation metric catalog. They record total simulation and analysis wall time, measured phase
times, configured worker count, build target and software version, available logical CPUs, CPU
model and host memory when exposed by the OS, and network, population, vehicle, and expected-leg
counts. Each CSV value includes its provenance.
Peak process memory is sampled after simulation and before analysis, and is included only when
the host provides a measurement; an unavailable measurement does not affect analysis. Reanalysis
preserves the original simulation context and refreshes the analysis timing. Cross-run metric
comparisons continue to use each run's latest
completed iteration and do not aggregate runtime metadata as simulation output. Legacy reports
without execution context retain unknown historical fields instead of inventing zero values.


## Modeled emissions

Configure `output.analysis.emissions` to consume externally modeled emission records; this
analysis does not calculate emissions. The CSV must include
`iteration,time_seconds,pollutant,unit,value,vehicle_id,link_id,area_id,emission_type`. Set exactly one of `link_id` and `area_id`
on each row, and use `warm` or `cold` for `emission_type`. Units are kept as supplied, so different
units are never summed together. `vehicle_id` joins through the run's vehicle catalog to a vehicle
type ID; `vehicle_categories` maps those type IDs to reporting categories. Only records for the
completed iteration are included. Totals use fixed 3600-second hours regardless of
`analysis.interval_seconds`. Values are expanded by `1 / qsim.sample_size`; both sampled and
expanded totals are exported.

```yaml
output:
  analysis:
    emissions:
      records: emissions.csv
      vehicle_categories:
        vehicle-type-car: passenger_car
        vehicle-type-truck: heavy_truck
      fleet_provenance: "vehicle categories from fleet-v3"
      emission_factor_provenance: "HBEFA 4.2"
      accounting_boundary: "tailpipe"
```

The report exports `emissions_hourly.csv` and `emissions_provenance.json`; link and area IDs are
the location keys for mapping these totals onto supplied network or area geometries. Provenance
records warm/cold record coverage, sample expansion, fleet and factor source, and the accounting
boundary. These values describe emitted mass, not concentration or exposure. Missing records for
the final iteration make `modeled_emissions` unavailable rather than reporting zero emissions.

## Journeys and travel distributions

The final-iteration replay exports observed legs in `legs.csv` and planned journeys in
`journeys.csv`. A journey spans consecutive substantive activities. Activity types containing
`interaction` are treated as stage activities, so transit access, egress, and transfer waits stay
inside one journey. Journey duration is elapsed time from the first observed component departure
to the last component arrival; incomplete journeys keep their mode and distance but have no duration.
The purpose is the destination activity type.

The main mode uses MATSim's default analysis hierarchy: the highest-ranked component mode wins,
which folds walk-transit-walk and transit transfers into one transit journey. An entirely walked
journey remains walk. When a custom mode is mixed with a known non-walk mode, the report marks the
main mode `unknown_mixed_modes` rather than guessing at a hierarchy the run has not configured.
`journey_mode_share.csv` groups journey counts and shares by departure
interval, purpose, distance class, and main mode. Distance classes are under 1 km, 1–5 km, 5–10 km,
10–25 km, 25 km or more, and unknown. `journey_summary.csv` reports count, completion, and mean,
population standard deviation, median, and 90th percentile for duration and distance by main mode
and destination purpose. Percentiles use the nearest-rank definition.

Journey distance sums the prepared plan's route distance for every component leg. This includes
model-derived distances assigned to teleported routes during plan preparation. If any component
has no finite non-negative route distance, the total is blank; `distance_provenance` distinguishes
`planned_route`, `partial_planned_route`, and `unavailable`. Component leg indices and modes
link each journey to `legs.csv`. A journey with no observed components is `not_departed`; observed
journeys distinguish `completed`, `stuck`, `missing_arrival`, and `incomplete`. These tables
describe the recorded selected plan and replayed events of the latest completed iteration only.

To compare saved journey mode shares, run `analyze --run-dir RUN --compare-run-dir OTHER`; repeat
`--compare-run-dir` for more runs. The command refreshes RUN's latest-iteration report, reads each
supplied run's recorded `journey_mode_share.csv`, and writes a local comparison report and combined
table under `RUN/analysis/cross_run_comparison`. A comparison refuses a run whose report is failed
or whose recorded iteration is not its latest output iteration.

## Daily activity patterns

`activity_durations.csv` holds one row per observed activity interval, and
`activity_patterns.csv` one row per person for the simulated day. Activity times come from the
recorded `actstart` and `actend` events; travel time comes from the observed leg completions in
`legs.csv`. The chains are the activity types in the order they were observed, the observed leg
modes in departure order, and the main modes of the observed journeys in order.

A stage activity is one whose type contains `interaction` — a transit access, egress or transfer
wait. `activity_durations.csv` flags these in the `stage` column and still reports them, so a day
reconciles against every observed interval, but neither the plan's activity count nor the
observed `observed_activities` counts them. Both sides therefore use the same rule as the journey
definition, and a transfer that happened can never stand in for a destination that did not.

### First and last day censoring

The recording window opens at `qsim.start_time`, so an activity observed to begin exactly there
was already in progress when the window did. Its total duration is only a lower bound and it is
flagged `start_censored`. An activity with no observed end by the time the run shuts down is
flagged `end_censored` for the same reason. Both flags are reported per activity and counted per
person, and a censored interval never reaches a duration mean. Every boolean in these tables —
`start_censored`, `end_censored`, `stage` and `crosses_zone_boundary` — is written as `true` or
`false`. The window start is recorded in
the run metadata, because the event files only contain events from the window onwards and cannot
recover it. A start time after the end time is refused rather than reported.

Censoring is separate from what is observable *inside* the window. A left-censored activity still
contributes the seconds it was observed to last, reported as `in_window_seconds` alongside the
unbounded `duration_seconds`. That is what lets a person's day be reconciled:
`activity_seconds + travel_seconds` equals `observed_span_seconds`, the time between the first and
the last thing the recording caught the person doing, and `timeline_gap_seconds` is whatever the
recorded events leave over. A nonzero gap is time that neither activities nor completed legs
account for. The bounds count every observed moment: an activity start or leg departure for the
first, and an activity end, leg arrival or stuck event for the last, so a person who was recorded
mid-leg and aborted keeps the aborted travel in the day and reports it as a gap.

### Pattern status

`status` is the first of these that applies, so the reason a day is short is the actionable one:

- `not_observed`: the person emitted no activity event at all in the window, so there is no
  observed day to classify. The plan and the observed events are separate sources and neither is
  assumed to exist.
- `stuck`: the person emitted a stuck event, so the rest of the day never happened.
- `truncated`: fewer activities were observed than the recorded selected plan contains, so the
  day stopped short of the plan.
- `complete`: every planned activity was observed. A last activity without an observed end is
  still `complete`; that is ordinary end-of-day right-censoring and is reported by the censoring
  flags rather than by downgrading the pattern.

The pattern totals are the ones a reader checks the day against: one pattern row per person, and
the cohort's journey count equal to the journeys in `journeys.csv` that departed, its travel
seconds equal to the completed legs in `legs.csv`, and the zone rows' origin and destination
counts both total the journey table.

`activity_type_summary.csv` groups activities by type and reports the uncensored count, both
censoring counts, and the mean, median and in-window total. `activity_pattern_summary.csv` is a
long-format table over three groupings — all persons, the supplied person zone, and the pattern
status — so one table covers the cohort, geographic and completeness views.

## Zones and origin-destination flows

`output.analysis.zone_system` is the documented zone system the geographic reports are built
from. It carries an optional `name` for provenance and two independent geographies, because a
location can be described by either:

```yaml
output:
  analysis:
    enabled: true
    zone_system:
      name: berlin-2018
      # External link ID to zone ID. Journey origins, journey destinations and activities are
      # located through the links their recorded events and plans name.
      link_zones:
        link-1: zone-a
        link-2: zone-b
      # External person ID to zone ID, the person geography the summaries are grouped by.
      person_zones:
        person-1: zone-a
```

Zones are supplied, never inferred from the network. A link with no entry and a person with no
entry are both reported as `unmapped`, and they still form OD rows, boundary crossings and zone
rows, so a partial zone system still accounts for every observed journey instead of reporting a
matrix that quietly adds up to less than `journeys.csv` does.

`zone_od.csv` is the mode and departure-interval keyed OD matrix: one row per
`(departure_hour_seconds, mode, origin_zone, destination_zone)` cell, with
`crosses_zone_boundary` and the distinct people behind it. `zone_flows.csv` is the boundary
crossing report: the same journeys collapsed onto the unordered zone pair, named
`min|max` so both directions of one boundary share a name. `zone_summary.csv` reports per-zone
link, resident-person, observing-person, activity and journey counts.

Only journeys with an observed departure enter the matrix, because the matrix is keyed by
departure interval and a journey that never moved has no interval to place. Those journeys keep
their zone counts in `zone_summary.csv` and their row in `journeys.csv`.

Without a zone system the three zone tables are published with their headers only, and the
`zones` module reports itself `unavailable`.

`urban_area_summary.csv` is grouped by the geography the run supplied, which the `geography`
column names on every row. A configured zone system is the report's own definition of an urban
area, so the summary is keyed by it and the person geography contributes `residents` and
`unmapped_residents`. Without one it falls back to the link classification the report already
computes, and the residents are zero because no person geography was supplied. Either way an
activity whose link the report cannot place is counted as `unclassified` rather than attributed
to an area it does not belong to, and every location is reported. The report embeds a bounded
preview of `activity_patterns.csv`, `activity_durations.csv` and `zone_od.csv` and says so when a
table is longer, as it does for `legs.csv`.

The supplied zone system is recorded in `manifest.json`, so a standalone rerun rebuilds the same
geographic report.
## Public transport performance and demand validation

Public transport is simulated: transit vehicles drive through the network, board and alight
passengers, and compete with every other vehicle for capacity. One passenger trip is rebuilt from
the events that record it: the run a vehicle starts (`TransitDriverStarts` names its line, route
and departure), the stop a passenger waits at (`waitingForPt`), the boarding
(`PersonEntersVehicle`) and the alighting (`PersonLeavesVehicle`). A run always starts before
anyone boards it, so the run in place when a passenger boards is the run it rode. A
`travelled with pt` event, which an earlier build wrote when passengers teleported, is still read
so that an event file recorded before vehicle simulation keeps producing its tables.
`service_modeling` in `transit_trips.csv` says which of the two a leg used: `simulated` for a
ride recorded by the vehicle events, `teleported` for a `travelled with pt` record. The transit
tables also use the person departure, arrival and stuck events, and the schedule and vehicle
capacities recorded in `run_metadata.json` (`transit`). The run's vehicle file is the only
capacity source: a departure's `vehicleRefId` is looked up in it, and the vehicle type's
`<capacity>` (seats plus standing room) is the capacity. Both XML and protobuf vehicle files
carry it.

`transit_trips.csv` has one row per passenger transit leg. `service_modeling` is `simulated`
for a leg with a service record and `unrecorded` otherwise. `outcome` is `boarded`,
`missed_service` (the service record's boarding time precedes the passenger's departure, so the
record cannot describe that leg), `no_service_record` (the leg arrived with no service record),
`stuck` or `incomplete`. Waiting is the boarding time minus the passenger's departure; in-vehicle
time is the arrival minus the boarding time. A missed service has no waiting time. Arrival delay
is the arrival minus the scheduled arrival at the egress stop of the departure the run names, so
it measures the delay the vehicle actually accumulated. Vehicle-level stop delay and missed stops
would need the vehicles' own `VehicleArrivesAtFacility` and `VehicleDepartsAtFacility` events,
which this module does not read, so they are always unavailable. A passenger who waits for a
later departure after missing one is indistinguishable from a passenger whose vehicle was late
without those events, so `missed_service` stays a record-consistency check.

| Table | Content |
| --- | --- |
| `transit_stop_hourly.csv` | boardings and alightings per interval, line and stop |
| `transit_line_summary.csv` | trips, missed services, mean waiting, in-vehicle time and delay per interval, line and route, with the number of observations behind each mean |
| `transit_occupancy.csv` | passengers, capacity and load factor per scheduled departure and route segment |
| `transit_journeys.csv` | access, egress, transfer, waiting and in-vehicle time per journey that uses transit |
| `transit_outcomes.csv` | trip outcomes per interval and service modeling |
| `transit_availability.csv` | which metric groups are available and why not |

Trip outcomes use the interval of the passenger's departure, line summaries and boardings the
interval of the scheduled boarding time, and alightings the interval of the arrival. A trip
with outcome `missed_service` still counts in boardings, alightings, line trips and occupancy,
because the simulation carried the passenger on that run; only its waiting time is unavailable.
An omitted `seats` or `standingRoom` element contributes zero persons, and a `<capacity>`
element naming neither declares no capacity.

An interval is the one containing the boarding time for boardings and the arrival time for
alightings. Counts are expanded by the reciprocal of `qsim.sample_size`; the `_sample` columns
keep the simulated counts. A load factor is the expanded passenger count divided by the vehicle
capacity, so it can exceed one. A journey's access and egress are the legs before the first and
after the last transit leg; the transfer time sums the time between leaving one vehicle and
boarding the next, walking and waiting included. The journey waiting time sums the wait at every
boarding, so a transfer wait is part of both the waiting and the transfer time. Any quantity whose inputs are missing (no
service record, no schedule, no matching departure, no capacity, an unfinished component leg) is
blank and listed as unavailable; it is never inferred, and a journey with a blank quantity has
status `incomplete`.

Set `output.analysis.transit_observed_data` to a CSV of observed demand to compare it with the
simulated boardings and alightings. Relative paths are resolved from the run's output directory.

```csv
scope,line_id,stop_id,station_id,period_start_seconds,period_end_seconds,metric,unit,value,source
stop,,1,,0,3600,boardings,persons,120,counter-a
line,Blue Line,,,0,3600,alightings,persons,800,survey
station,,,central,0,3600,boardings,persons,300,gate-counts
line_stop,Blue Line,3,,0,3600,alightings,persons,90,survey
```

`scope` is `stop`, `station` (the stop facility's `stop_area_id`), `line` or `line_stop`, and
the matching id columns have to be set. `metric` is `boardings` or `alightings` with unit
`persons`, `person` or `passengers`. The period has to be exactly one analysis interval.
`transit_validation_matches.csv` carries the observed value, the simulated sample count, the
expansion factor, the expanded simulated value, the residual, the relative error (blank when
the observation is zero), the network-wide simulated total of the same metric and interval as a
denominator, and the observation source with its row. Rows that cannot be compared, with a
reason such as `unknown_entity`, `period_mismatch` or `no_service_records` (no transit was
simulated, so not even a zero is compared), are in `transit_validation_unmatched.csv`.
`transit_validation_summary.csv` gives matched and unmatched counts, observed and simulated
totals, bias, MAE, RMSE and the relative bias per scope and metric. An invalid file marks only
the `transit_validation` module failed.

`compare_completed_runs` compares transit stop, line, outcome, occupancy, journey, and observed-demand
metrics using the aggregation keys in each run's catalog. It reads each supplied run's latest
completed iteration; metrics missing from either run remain unavailable in the comparison report.

## Link speeds

`link_speed` reconstructs traversal speeds from the same replay that produces the link
volumes. A speed observation needs a *full-link* traversal: the vehicle has to enter a
link at its start and leave it at its end. QSim records the first link of a network leg
with `vehicle enters traffic` and the last one with `vehicle leaves traffic` instead of
`entered link` and `left link`, and both carry a relative position along the link, so a
vehicle inserted at the end of its start link covers no distance. Such traversals are
reported as partial records rather than folded into the statistics, which is a
deliberate deviation from the MATSim link-speed analysis.

An observation is assigned to the interval in which the vehicle *entered* the link, so
a traversal crossing an interval boundary stays whole in its entry interval. Because an
entry is what starts a traversal, every interval that can hold a speed also holds a link
entry, and volume, coverage, group and speed tables therefore share one interval list.

`link_speed_hourly.csv` holds the per-link statistics, `link_speed_summary.csv` the
across-link mean and population standard deviation per interval,
`link_speed_histogram.csv` the fixed-bin distribution, and
`link_speed_diagnostics.csv` every record that could not produce a full-link speed.


## Observed validation

Set `output.analysis.observed_data` to a CSV file to compare observations with the latest
completed iteration. Relative paths are resolved from the run's output directory; standalone
reanalysis reuses the path recorded in the manifest. The file must contain one row per link and
period, with these exact headers:

```csv
link_id,period_start_seconds,period_end_seconds,vehicle_class,metric,unit,value,split
link-1,0,3600,all,count,vehicles,120,calibration
link-1,0,3600,all,speed,km/h,36,holdout
```

`link_id` is the external network link ID. Periods must match the configured analysis interval
exactly, and each split may contain only one row per link, period, class, and metric.
`vehicle_class` accepts `all` for aggregate results or a vehicle type ID from the run's
vehicle definitions. Class-specific count and speed tables are exported alongside aggregate link
tables. The class name `all` is reserved for aggregate observations. `metric` accepts `count` or
`speed`; count units are `vehicles`, `vehicle`, or
`veh`, and speed units are `m/s`, `mps`, `km/h`, or `kph`. Counts are expanded by the reciprocal
of `qsim.sample_size`, while speeds are not expanded. `split` is `calibration` or `holdout`. An
optional `source` column can identify a station or data source; the path, label, and source row
are carried into matched and unmatched exports.

The report exports matched rows, unmatched input rows, and bias, MAE, RMSE, and count GEH in CSV,
grouped by split, metric, and vehicle class. GEH scales matched count intervals to hourly rates before applying the formula, so
its thresholds remain comparable when `interval_seconds` differs from 3600. Relative error is
blank when the observed reference is zero. It also writes separate
calibration and holdout scatterplots by metric, time profiles, and residual maps. These plots use
aggregate `all` observations so vehicle classes are not counted again alongside the aggregate.
The input path is recorded as
observation provenance in each matched row. Validation input errors leave the core report intact
and mark only the validation module failed.

To compare completed runs, set `output.analysis.comparison_runs` to run output directories. Each
directory's `analysis/manifest.json` selects its latest completed iteration, and its published
aggregate and vehicle-class count and speed tables are combined in `cross_run_comparison.csv`.
Count rows include both the simulated sample and the population-expanded value, using each run's
recorded sample size; speed rows have identical sample and population values. Each row includes
the full period start and end, so runs with different interval widths remain identifiable.
Relative paths are resolved from the current run's output directory. A missing or incomplete
comparison report marks only the cross-run comparison module failed.


## Modeled noise and exposure

Set `output.analysis.noise` to analyze supplied receiver records; this reads model outputs and
does not add a noise simulation engine.

```yaml
output:
  analysis:
    noise:
      records: noise.csv
      affected_population: affected_population.csv  # optional
```

`noise.csv` has `receiver_id,period_start_seconds,period_end_seconds,metric,unit,value` columns;
optional `x,y` columns give receiver coordinates in the supplied map coordinate system.
Metrics named `source_sound` and `exposure` require `dB` and are combined by energy mean when
multiple records share a receiver, period and metric. A supplied `damage` metric is summed in its
input unit; no monetized damage is calculated. Other supplied metrics use an arithmetic mean.
`affected_population.csv` has `receiver_id,period_start_seconds,period_end_seconds,affected_population`;
values join only on the exact receiver and period, and duplicate rows sum. Receiver maps are
written per sound/exposure metric and exact period only when coordinates are supplied; `noise_maps.csv`
indexes them. Without that file the
population column stays blank and availability says unavailable. Summary and availability tables
are exported and included in the local report. Cross-run comparison reads the latest report's
noise summary.

## DRT and taxi service performance

Set `output.analysis.service` to analyse supplied DRT or taxi records. Nothing is simulated: the
module only reads CSV files, so it needs no service engine. Relative paths are resolved from the
run's output directory and the settings are recorded in the manifest for standalone reanalysis.

```yaml
output:
  analysis:
    service:
      requests: requests.csv          # required
      passengers: passengers.csv      # optional
      fleet: fleet.csv                # optional
      schedule: schedule.csv          # optional
      max_wait_seconds: 600           # optional constraint
      service_area: [[0, 0], [1000, 0], [1000, 1000], [0, 1000]]  # optional polygon
```

- `requests.csv`: `request_id,submission_seconds,origin_link,destination_link` plus optional
  `person_id,status,group,direct_travel_seconds,party_size`. `status` is empty/`submitted` or
  `rejected`. **A request is rejected only if its request record says so**; it is never inferred
  from missing legs. A non-rejected request without a passenger record is `unserved`.
- `passengers.csv`: `request_id,vehicle_id,pickup_seconds,dropoff_seconds`, one association per
  served request. Rows for unknown or rejected requests, duplicates, and impossible times are
  excluded and listed in `service_diagnostics.csv`.
- `fleet.csv`: `vehicle_id,capacity,service_start_seconds,service_end_seconds`.
- `schedule.csv`: `vehicle_id,task_type,start_seconds,end_seconds,distance_meters` where
  `task_type` is `drive`, `stop` or `stay`. Only `drive` rows carry distance.

Wait is pickup minus submission; the detour ratio is in-vehicle time divided by
`direct_travel_seconds` and is blank without it. Distributions use the mean, population standard
deviation, median and 90th percentile, taken at index `ceil((n - 1) * q)` of the sorted values
(the same helper as the journey tables, so the median of an even count is the upper middle value). Pickups and drop-offs happen at stops, so a drive task is occupied by the
served requests on board at its midpoint; its load is the sum of `party_size` on board, so shared
rides and groups both count. Empty distance is driven distance
with load zero, which includes relocation. Mean occupancy is passenger-metres over driven metres,
load factor divides passenger-metres by capacity-metres (for this ratio only vehicles with a
fleet capacity contribute to either side), and utilization is non-`stay` task time clipped to each vehicle's fleet
service window over that window.

Coverage is the share of requests whose origin and destination links are entirely inside
`service_area`; links crossing the border or absent from the network count as outside or
`area_unknown`. Group rows appear in `service_summary.csv` only when requests carry `group`
labels; blank labels are grouped as `unknown`. Configured wait and fleet capacity constraints are
recorded in `service_constraints.csv` and checked as `wait_limit_exceeded` and
`capacity_exceeded_tasks`. A metric whose input was not supplied is blank, and
`service_availability.csv` names the missing input. Invalid input fails only the
`service_performance` module. The completed-run comparison also compares the service summary,
vehicle and occupancy metrics from each run's published latest-iteration report, keyed by group,
vehicle and passenger load as appropriate.

Tables: `service_summary.csv`, `service_requests.csv`, `service_vehicles.csv` (a `fleet` total row
first), `service_occupancy.csv`, `service_constraints.csv`, `service_availability.csv`,
`service_diagnostics.csv` (which also lists non-positive `direct_travel_seconds`, vehicles missing from a supplied fleet, and fleet windows that end before they start). All appear in the local report.
## Travel survey comparison

Set `output.analysis.journey_survey` to a weighted journey-record CSV. Relative paths are
resolved from the run output directory and recorded in the manifest for standalone reanalysis.
Each row must contain `study_population`, `journey_definition`, `split`, `mode`, `purpose`,
`departure_seconds`, `duration_seconds`, `distance_meters`, and `weight`; `uncertainty` is
optional. The study population must be positive and identical across rows. Weights and
uncertainty must be finite and non-negative. Split is `calibration` or `holdout`.

The comparable journey definition is `matsim-substantive-activities-v1`: consecutive
substantive activities form a journey, interaction activities remain inside it, main mode uses
the MATSim hierarchy (including transit), and purpose is the destination activity type. Survey
mode labels must use the same categories as the simulated `main_mode`. Other
definitions are retained in the output but marked `non_comparable_definition`, with observed
shares withheld. The output compares weighted mode and purpose categories, departure hour,
distance class, and duration class. Departure uses fixed clock hours; distance uses the journey
classes above; duration bins are under 15, 15–30, 30–60, 60–120, and 120 or more minutes.
Duration distributions use survey records with a duration and simulated journeys marked
completed. Calibration and holdout refer to survey records; the same simulated distribution is
shown against each split. The `journey_survey_comparison.csv` table
includes observed and simulated denominators, shares, split, population, supplied uncertainty,
and missing-category status. `uncertainty` is the standard error for that record's weight; the
reported group uncertainty combines weighted record standard errors in quadrature. Its rows are
embedded in the local report. Input errors fail only this optional module.

## Network distance, time and congestion

`network_distance_time.csv` reports observed vehicle link traversals by link and interval;
`network_distance_time_summary.csv` reports network totals by interval. Traversal distance is
`link.length * (exit_position - entry_position)`, so first and last link portions count. Visits
with entry position zero and exit position one are complete; other valid forward positions are
partial and included. Positions outside `[0, 1]` or a decreasing position, non-finite or negative
lengths and decreasing event times are excluded and counted in
`network_distance_time_diagnostics.csv`. A valid visit with zero elapsed time retains its observed
distance and traversal count, reports zero vehicle time and signed delay when the reference speed
is valid, and contributes no relative-speed ratio; it is counted in the non-positive-duration
diagnostic. Visits still open at the end are counted as unfinished and contribute no guessed
distance or time. A same-link route is a regular visit and is counted when its entry and exit
events are paired.

Vehicle time is the elapsed time between link entry and exit. A visit crossing an analysis
interval boundary is assigned whole to its entry interval, as in the link-speed tables. This
avoids assuming how the vehicle moved inside the link. Distance and signed free-flow-relative
delay are kept with that visit. Free-flow-relative delay is `observed time - distance /
freespeed`, so it can be negative. `relative_speed_ratio` is free-flow travel time divided by
observed time, so values below one indicate slower than free flow and values above one indicate
faster travel. Network ratios use the sums over visits with valid free speeds. A non-finite or non-positive free speed leaves delay blank for
that traversal while retaining its distance and time. The optional
`output.analysis.excess_delay_clip_seconds` setting exports separately labeled clipped excess
delay: per link and interval it sums positive traversal delay, then caps that sum at the configured
value. The network total sums those capped per-link values, so it matches the per-link export.
The default is unset, in which case the clipped-delay column and catalog entry are omitted.
It can be set in YAML or with `--set output.analysis.excess_delay_clip_seconds=120`.

The event stream records vehicles and person travel events but does not provide reliable
link-level passenger occupancy. Passenger distance and time are therefore explicitly unavailable;
PCE is a capacity weight and is not treated as a passenger count. Existing leg departure and
completion metrics provide agent travel profiles. `en_route_agents.csv` counts distinct people
with an observed departure not yet matched by an arrival or stuck event, reports departures,
arrivals, stuck events, interval-start and peak concurrent counts, and apportions person-seconds
from event timestamps. It covers travel modes in the person event stream and does not claim
link-level network occupancy. Link speed tables remain a
separate view and only include complete full-link traversals.


## Link classification

`output.analysis.link_labels` is a map keyed by external link ID. Each entry can
provide independent `road_type` and `road_size` strings. If no geographic
boundary is configured, it may also provide `urban_area`. Missing and blank
labels are exported and grouped as `unknown`; supplied category spelling is
preserved.

`output.analysis.urban_boundary` is an optional polygon encoded as a list of
`[x, y]` coordinates in the same coordinate system and units as network node
coordinates. It needs at least three finite points; the last point is connected
to the first automatically. Points on the polygon boundary count as inside, to
within floating-point rounding. Assignment uses the link's endpoints: both inside
is `inner` even if the segment leaves a concave polygon, both outside is `outer`
unless the segment crosses or touches the polygon, and exactly one inside is
`cross_boundary`. When a polygon is supplied, it determines urban labels instead
of per-link `urban_area` labels.

Classification is report metadata only; it does not filter the eligible network.
Group coverage reports each category's fixed eligible-link count and its used
and unused counts for every hourly interval. The three dimensions are
aggregated independently so links with incomplete labels remain visible.

The report also provides independent urban-area, road-type, and road-size
filters. They update the per-link hourly metrics, group coverage table, and map
together. A filtered group table recounts eligible links within the selected
subset, so its rows always describe exactly the links on screen;
`group_coverage.csv` is unaffected and keeps the full-network denominators.

The filters need the per-link hourly rows in the page, so `index.html` embeds one
JSON row per link and interval, roughly 260 bytes each. That is negligible for a
small network and grows to tens or hundreds of megabytes for a large one (about
300 MiB at 50,000 links over 24 hourly intervals), which browsers handle poorly.
`link_hourly.csv` carries the same data compactly and is the artefact to read for
a network of that size.

The local SVG map marks links used at least once during the final iteration in
green and unused links in gray. Dashed lines identify expressways, which means
the exact, case-sensitive label `expressway`; any other road type is drawn solid.
Hover over a map link to see its labels and usage.

The same urban area is the grouping of `urban_area_summary.csv`, which adds the activities
and journeys each area carries. See "Zones and origin-destination flows" above.
## Comparing completed runs

The shared analysis interface can compare existing reports with an explicit baseline:

```rust,ignore
use rust_qsim::simulation::analysis::compare_completed_runs;
use std::path::{Path, PathBuf};

let report = compare_completed_runs(
    Path::new("runs/baseline"),
    &[PathBuf::from("runs/alternative-a"), PathBuf::from("runs/alternative-b")],
)?;
```

Each input must contain a complete `analysis/manifest.json`, metric catalog and latest-iteration
tables. The comparison is written to `baseline/analysis/comparison/` and leaves each input report
unchanged. `metric_differences.csv` exports both values, alternative-minus-baseline difference,
relative difference, unit, aggregation key, the baseline value used as the relative denominator,
and metric-specific aggregation denominators where the source provides them. Relative differences
are blank when the baseline is zero. `metric_compatibility.csv` identifies
missing metrics, incompatible definitions and unavailable or unregistered outputs. Link rows are
matched by external link ID; links missing from either network are excluded and the number of
corresponding links appears in the HTML report. Aggregate network and group metrics are omitted
when the link sets differ; network-wide speed and V/C distributions are omitted too, while
per-link outputs retain only corresponding IDs.
Runs with different interval widths, simulation end times, sample-size scales, or link
classification/filter definitions are rejected. `completion_status_differences.csv` and
`completion_status_transitions.csv` show policy-induced changes in complete, incomplete, stuck and
no-travel populations; `leg_completion_status_transitions.csv` reports changes per person and leg.
Per-person duration comparisons include only people with a complete plan in both runs. Leg-hour and
daily-cohort aggregates are recomputed over their common complete populations. Other registered
link, group, capacity and speed outputs are compared by their catalog aggregation keys. The HTML
report renders the metric and completion-status tables.

## Seed uncertainty and parameter sensitivity

Use `analyze --run-dir BASELINE --ensemble-manifest ensemble.json` to summarize completed runs
without launching simulations. The manifest names a baseline scenario and each run's output
directory and scenario; paths are relative to the manifest. Alternative runs may also declare a
`parameters` object, whose canonical value identifies a parameter setting:

```json
{
  "baseline_scenario": "baseline",
  "pairing": "paired_by_seed",
  "runs": [
    {"run_dir": "runs/base-1", "scenario": "baseline"},
    {"run_dir": "runs/policy-1", "scenario": "policy", "parameters": {"toll": 1.0}}
  ]
}
```

The report is written to `BASELINE/ensemble`. It exports the member and pair manifests, missing
pairs, per-seed metric differences and their distributions. Differences use the shared completed-run
comparison interface and each run's latest completed iteration. Both `paired_by_seed` and
`difference_of_means` uncertainty assumptions are shown; the manifest's optional `pairing` chooses
which result is marked as supplied. Equal seed numbers alone do not guarantee comparable random
streams. Intervals are two-sided 95% Student-t intervals; one pair has no interval. Quantiles use
the nearest-rank convention. Metrics may be restricted with an optional `metrics` array.

For example:

```yaml
output:
  analysis:
    enabled: true
    link_labels:
      link-1:
        road_type: expressway
        road_size: large
    urban_boundary:
      - [0.0, 0.0]
      - [1000.0, 0.0]
      - [1000.0, 1000.0]
      - [0.0, 1000.0]
```

## Accessibility to supplied opportunities

The `accessibility` module reports how many jobs, schools or services each origin
can reach within a travel-time threshold. It stays `unavailable` until all three
of its inputs are configured, and a partial set or an unreadable file marks only
this module `failed`. Relative paths resolve from the run's output directory.

The measure is declared rather than implied: every exported row carries
`measure=cumulative_opportunities_within_threshold`. It sums the weight of every
supplied opportunity whose **potential** travel cost from the origin is at or
below the threshold, and the threshold is inclusive, so a destination costing
exactly the threshold counts as reachable.

Realized trips are not potential destinations. A journey table says how long one
person actually took, which says nothing about how long anyone else *could*
take, so `legs.csv` and the journey tables are never substituted for the supplied
costs. Where a cost is missing, the module reports no value rather than deriving
one from what happened to be travelled.

The three inputs, all CSVs with these exact headers:

```csv
# opportunities: one row per location
opportunity_id,category,x,y,count
job-1,jobs,100.0,200.0,250

# zones: the explicit coordinate/zone correspondence
zone_id,x,y
zone-1,0.0,0.0

# travel_costs: potential-destination costs, by mode and departure period
origin_zone,destination_zone,mode,period_start_seconds,travel_time_seconds
zone-1,zone-1,car,28800,0
zone-1,zone-2,car,28800,1800
```

`x` and `y` are in the same coordinate system and units as network node
coordinates. Weights and costs must be finite and non-negative, and a duplicate
location, zone or cost key is a module failure. An opportunity or a person is
placed in the zone whose centroid is nearest by horizontal distance, with ties
broken on the zone id, so the assignment does not depend on the input file's row
order. A person is placed by their first non-stage activity. A blank category is
reported as `unknown` rather than rejected. Cost rows naming a zone the zone file
does not list are counted in `accessibility_diagnostics.csv` instead of failing the
module, because a rectangular skim is routinely wider than the zones under study.

`accessibility_zones.csv` holds one row per origin zone, category, mode, departure
period and threshold. `status` distinguishes five outcomes, because folding them
together would misreport the data:

| `status` | meaning |
|---|---|
| `available` | every location of the category has a supplied cost from this origin |
| `available_missing_costs` | some locations have no cost; they are excluded and counted in `opportunity_locations_without_cost` |
| `unavailable:no_origin_costs` | no cost of this mode and period leaves the origin |
| `unavailable:no_travel_costs` | the cost file supplies no table for this mode and departure period |
| `unavailable:non_finite_measure` | the category's weights summed to a non-finite number |

A person whose plan has a home activity that cannot be placed gets
`unavailable:no_home_zone` rows rather than disappearing from the population. A
person with no selected plan has no recorded expectations at all and so is not in
`accessibility_persons.csv`; the run's `expected_travel` list is what both the
per-person and per-zone tables are built from. Every unavailable status leaves the
measure columns blank rather than reporting zero, so a missing prerequisite is
never read as poor accessibility.

`accessibility_summary.csv` reports, per category, mode, period and threshold, the
mean, median, minimum and maximum over the zones that have a supplied cost, plus a
`population_weighted_opportunities` column. The two differ exactly when
opportunities are unevenly distributed over people, which is the equity signal.
`persons_included` is the simulated person count the weighting covers, and
`sample_size` is the simulated fraction of the population they are, so a reader can
tell a sampled run from a full one. A zone with no supplied cost at all is counted
in `zones_without_costs` and excluded from the statistics, because treating it as a
zone holding zero opportunities would drag every mean down. A zone in
`available_missing_costs` *is* included, so `zones_without_costs` counts only fully
unavailable zones; read the missing-cost share from the zone table.

`accessibility_persons.csv` repeats the value of each person's own origin zone per
cell, which is what an equity analysis reads. Following the catalog rule in
`docs/architecture.md`, every accessibility name in `metric_catalog.json` is a
column of one of the tables above, so a consumer can look it up where it is
exported. The measure itself is catalogued as `opportunities`, and the declared
measure name is in each row's own `measure` column, which is what tells a consumer
which definition a value was computed under. The `aggregation_key` names the
origin, category, mode, period and threshold, which is how a comparison tool lines
two runs up.

`accessibility_map.svg` draws one small panel per reported cell on a shared
projection, up to 24 panels; further combinations stay in the CSVs and
`map_panels_omitted` counts them. A filled circle is an origin zone shaded across a
single-hue ramp normalized to its own panel, a gray circle is an origin with no
supplied cost, and a green ring is a zone holding opportunities of that category,
sized by their total weight. The per-zone and per-person tables are embedded in
`index.html` as bounded previews of 500 rows, because a per-person table over several
categories, modes, periods and thresholds outgrows a page; the summary and
diagnostics tables are embedded in full because each holds one row per reported
combination rather than per person. The CSVs hold every row either way.

For example:

```yaml
output:
  analysis:
    enabled: true
    accessibility:
      opportunities: accessibility/opportunities.csv
      zones: accessibility/zones.csv
      travel_costs: accessibility/travel_costs.csv
      # Defaults to a 45-minute cutoff when omitted.
      thresholds_seconds: [1800, 3600]
```

Every setting is also reachable from the command line, for example
`--set output.analysis.accessibility.thresholds_seconds=1800,3600`.

## Demographic outcomes and equity

Set `output.analysis.person_group_attributes` to the person attributes the report groups people by, such as `income` or `age`. Missing or blank attributes are grouped as `unknown`. Optional weight and cost attributes are recorded with the run; invalid or missing weights default to one, while unavailable costs remain blank.

`group_burdens.csv` reports weighted group sizes and completed daily travel-time burdens. Incomplete or stuck people remain in group counts without lowering the travel-time mean. `person_demographics.csv` lists each person's groups, weight, and cost. `equity_comparison.csv` compares completed daily burdens with configured runs; differences within one microsecond count as unchanged, and persons without comparable completed days are reported separately.

`group_module_outcomes.csv` combines per-group outcomes exported by other modules using `<module>_group_outcomes.csv` with columns `dimension,group,metric,unit,value`.
