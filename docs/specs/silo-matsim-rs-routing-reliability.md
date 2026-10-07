# SILO matsim-rs routing reliability

## Problem Statement

SILO is being migrated from Java MATSim to matsim-rs for its computationally expensive traffic simulation and individual car and public transport (PT) routing. The current integration can complete matsim-rs QSim and serve route requests, but a 2% SILO run has not yet completed uninterrupted with the Rust route service available throughout. One run stalled while SILO awaited a route response during year-end modal-share reporting. Other requests returned no route and SILO substituted beeline travel time, making it hard to tell whether the origin-destination pair is genuinely disconnected or the Rust routing network, mode restrictions, or link selection are wrong.

The user needs a reliable run and enough evidence to trust that SILO is using matsim-rs for its core simulation and individual car/PT routes.

## Solution

Make the SILO–matsim-rs integration reliable and observable. Keep QSim and individual car/PT route calculations in matsim-rs. Ensure route requests either return a valid result or a bounded, actionable failure; a stalled request must not hang a SILO year. Preserve a beeline fallback for genuinely unroutable pairs so one disconnected pair does not abort the run, but count and report those fallbacks so they cannot be mistaken for successful Rust routing.

Diagnose and correct any mode/network/link mismatch responsible for avoidable route failures. Validate the complete behavior with an uninterrupted 2% SILO run through year-end reporting, with the Rust route service left running for the entire run.

## User Stories

1. As a SILO modeler, I want matsim-rs to run QSim, so that the most time-consuming traffic simulation work no longer runs in Java MATSim.
2. As a SILO modeler, I want individual car routes requested from matsim-rs, so that repeated network routing computations use the Rust implementation.
3. As a SILO modeler, I want individual PT routes requested from matsim-rs when PT is enabled, so that PT route computations also use the Rust implementation.
4. As a SILO modeler, I want car and PT routing to avoid silently falling back to Java MATSim, so that the selected engine is clear and the migration has a verifiable boundary.
5. As a SILO modeler, I want route requests to use links from the matching mode-specific network, so that a car or PT request is not rejected because SILO selected an incompatible link.
6. As a SILO modeler, I want Rust route results to include valid travel time and distance for routable requests, so that SILO can use the results in its transport decisions and reports.
7. As a SILO modeler, I want genuinely disconnected origin-destination pairs to be handled without aborting the SILO year, so that isolated cases do not stop a long simulation.
8. As a SILO modeler, I want every beeline fallback counted and reported by mode and reason, so that fallback use cannot make a failed Rust routing run look successful.
9. As a SILO modeler, I want routing failures to distinguish no-path results from invalid links, malformed requests, missing people, service errors, and timeouts, so that I can identify data problems separately from software defects.
10. As a SILO modeler, I want a stalled route request to fail within a bounded time and release its client connection, so that SILO can continue or report a controlled failure instead of hanging indefinitely.
11. As a SILO modeler, I want a timed-out or broken connection to recover for later requests when the Rust service is still healthy, so that one transient failure does not disable routing for the rest of the year.
12. As a SILO modeler, I want Rust routing to remain available during year-end reporting, including modal-share calculations, so that the route service lifecycle covers every SILO consumer that needs it.
13. As a SILO modeler, I want route-service startup and shutdown to be tied to the SILO run lifecycle, so that readiness is reliable and no orphan service remains after completion or failure.
14. As a SILO modeler, I want summaries to show how many requests Rust served, failed, timed out, and fell back by mode, so that I can judge whether the migration is working from a completed run.
15. As a SILO modeler, I want an uninterrupted 2% sample run from setup through year-end output, so that the end-to-end integration is validated at a practical scale before larger runs.
16. As a SILO modeler, I want repeat runs with the same inputs and seed to produce consistent routing outcomes, so that routing reliability does not undermine reproducibility.
17. As a SILO maintainer, I want route-service failures to include the request mode, link IDs, and failure category without dumping excessive person or household data, so that failures are diagnosable and logs remain usable.
18. As a SILO maintainer, I want compatibility behavior for same-link, missing-link, invalid-coordinate, disconnected-network, and PT-disabled requests to be explicit, so that edge cases do not become hangs or misleading successful routes.

## Implementation Decisions

- Keep the Rust QSim and persistent Rust route service as the execution path for the core simulation and SILO individual car/PT route requests.
- Keep Java-side SILO setup and permitted side work such as skim generation, event replay, and reporting outside the core QSim and individual car/PT route calculations.
- Do not use Java MATSim routing as a fallback for individual car/PT requests. For an unroutable pair, retain the existing beeline fallback only as an explicit, counted degraded result.
- Trace failed requests end to end: SILO location-to-coordinate conversion, mode-specific network selection, nearest-link selection, serialized request, Rust network membership and mode restrictions, and the Rust route result. Correct confirmed mismatches at their source rather than broadening allowed modes indiscriminately.
- Bound route response waits and make connection recovery safe after timeout, EOF, or service restart. A failed route must not leave a client connection in an unusable state or silently disable all future Rust routing.
- Keep the route service alive through all SILO phases that query travel times, including year-end modal-share reporting, and close it with the owning run.
- Surface per-mode request, success, no-path, invalid-request, service-error, timeout, and beeline-fallback counts in run logs or the existing run summary. Avoid logging full household/person records for each failure.
- Use the complete 2% SILO run as the primary acceptance seam. Add narrower route-service coverage only for failure categories that cannot be reliably triggered or isolated through that run.

## Testing Decisions

- The primary test observes the integration through SILO's existing run boundary: start from the 2% scenario configuration, let matsim-rs execute QSim and serve individual car/PT routes, and verify SILO reaches `Finished SILO` without manually stopping either process.
- The end-to-end run must verify that the Rust route service remains available through year-end modal-share calculations, and that the SILO process exits successfully with expected year outputs.
- The run must report Rust route successes, categorized failures, timeouts, and beeline fallbacks by mode. A completed process alone is insufficient if requests silently fell back.
- Route-service behavior should be tested through its request/response boundary: valid car/PT route, same-link request, absent link, invalid coordinate/time, no-path route, and unavailable or stalled service. Assert response category, bounded completion, and that a later request can still succeed after recoverable failures.
- SILO client behavior should be tested through the same public routing call used by the model: car/PT requests go to Rust; no Java car/PT fallback occurs; only an explicit beeline fallback is returned for a classified failure; counters and logs reflect the outcome.
- Tests should assert externally visible results and lifecycle behavior, not private helper methods, socket internals, or implementation-specific thread counts.
- Prior art is the existing Rust route-service code and routing tests in the Rust simulation crate, plus SILO's existing 2% scenario-run scripts and integration outputs. Prefer extending those seams over adding a parallel harness.
- Do not use the full Berlin integration runs for this SILO acceptance check; the target scenario is the Bangkok 2% SILO run.

## Out of Scope

- Replacing Java MATSim side work that is not part of QSim or individual car/PT routing, including SILO configuration/setup, skim/report generation, event replay, and other post-processing.
- Replacing every MATSim Java library type or removing Java MATSim dependencies from the SILO repository.
- Implementing unrelated Rust MATSim features not required by this SILO QSim and car/PT routing path.
- Changing SILO transport behavior to make a disconnected route appear connected, or broadening mode restrictions without evidence that the current network configuration is wrong.
- Calibrating Bangkok traffic speeds, mode shares, or demand beyond what is needed to validate the integration contract.

## Further Notes

- The prior 2% retry3 run reached `Finished SILO`, but only after the Rust route service was manually stopped during a long route-response wait; later requests used beeline fallback. It is not an uninterrupted acceptance run.
- Retry4 completed QSim and started the route service, but its log ended during SILO routing without a `Finished SILO` marker. It is not an acceptance run.
- The available evidence does not establish whether every `No route found` response is a legitimate disconnected pair. Link names such as `rail_*` or `bus_*` in car-mode failures are a lead for network/mode auditing, not proof of the root cause.
- A response timeout is containment, not a substitute for identifying why a request stalled.
