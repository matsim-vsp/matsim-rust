# Routing integration design synthesis

## Pick

Use candidate A's calculator-owned acceleration policy. Graft candidate B's immutable snapshot lifetime and strict equal-cost tie preservation. The existing `LeastCostPathCalculator` boundary remains the route call surface, so custom costs can retain baseline behavior without opting into cache or certified search capabilities.

## Candidate review

Candidate A fits `NetworkRoutingModule` and the current calculator ownership with fewer lifecycle changes. Candidate B's routing epoch makes snapshot ownership clear, but its controller service and publication stage add a new lifecycle across the controller and all routing modules.

The cross-judge preferred A on correctness, fit, interface depth, deterministic behavior, and incremental cost. The judge required three refinements. Keep historical priority ordering. Make old in-flight snapshot insertions safe. Apply adaptive decisions before copying or changing plan selection.

## Grafts and rejections

- Grafted B's pinned snapshot identity into every dynamic cache key.
- Grafted B's requirement that equal-cost candidate bounds remain searchable.
- Grafted B's guidance that proposal hints remain immutable while Rayon workers route.
- Rejected B's controller-wide epoch manager and per-graph scratch storage as additional lifetime machinery not required by existing calculator calls.
- Rejected candidate-based pruning for arbitrary dynamic costs because the current cost contract does not prove FIFO or heuristic consistency.

## Implementation order

1. Reusable search scratch with historical queue ordering.
2. Versioned exact cache behind explicit cost-cache capabilities.
3. Proposal validation and strict certified bounds.
4. Optional adaptive rerouting before plan cloning.
5. Stable batch proposal preparation and deterministic fallback.
6. Bounded shared-goal guidance, first as a validated proposal.

Each step adds behavior checks and records evidence in `.audit/katgpt-integration-opportunities.tsv` before the next begins.
