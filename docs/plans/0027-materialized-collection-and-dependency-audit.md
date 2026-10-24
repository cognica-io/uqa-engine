# Materialized collection throughput and dependency audit

The maintainer requested resolution of the remaining performance and dependency failures after the four shaped-row deliveries. This plan preserves both tasks: the current materialized-result regression tracked in #653 and the complete dependency audit failures tracked in #1262. UnifiedPlan, SQL evaluation and persistent formats remain unchanged.

## Live task records

| Task | Owner and delivery | Current evidence and remaining work |
| --- | --- | --- |
| #1262: dependency audit | Existing Storage cryptography, CLI terminal and HTTP/binding runtime dependencies; PR #1263 | Official ctutils 0.4.5, Tokio 1.53.3 and clipboard-win 5.4.3 replace vulnerable/yanked versions. Full cargo-deny passes without new exceptions; 964 Storage, 100 CLI and 81 HTTP client tests pass locally. Only three versions/checksums change. Final automatic Linux/macOS regression and affected FDW qualification, merge and closure remain. |
| #653: materialized SQL result throughput | Execution's query consumer/collection boundary; [PR #1264](https://github.com/cognica-io/uqa-engine/pull/1264), dependent on #1263 | The complete public-result consumer currently uses the same one-row producer demand as consumers that may stop early. The instrumented 16,384-row fixture reproduced 16,385 producer calls against the required 129 before the correction. Preserve ordinary producer batches for complete collection, row-level stop/error boundaries for other consumers, retained ownership, duplicate slots, late errors and cancellation. Local verification, final-source automatic checks and a controlled result under the original bounds remain required. |

- [ ] Deliver the complete dependency correction with actual final-source qualification and close #1262.
- [ ] Correct complete-result scheduling, preserve early-stop semantics and deliver the performance change with local and actual platform evidence.
- [ ] Inspect the next automatic controlled result, retain original failed observations and reconcile #653's current regression and historical investigation before closure.

## Current performance evidence

Automatic main run 39956647527 tests `a25cd0596eccd28fd066ecd6c5a3b8f7508c4b70`. Q1, Q6, cursor output and all claims comparisons are accepted. Materialized result collection remains a regression: ratio 1.878805, qualified interval [1.853123, 1.904842], unchanged maximum 1.15. The analytical workflow fails correctly and the reusable EC2 instance stops successfully. Reference revision, executable and workload identities remain those recorded in the shaped-row completion plan; no generated machine report belongs in Git.

The change to unconditional one-row demand entered collection in `16533ee6d` while fixing early-stop SQL semantics. Public SQL's final ResultRows consumer always continues to exhaustion, so that global demand adds a batch allocation and pipeline entry per row even when the producer supports bounded native batches. The pre-correction local test fails deterministically at 16,385 calls versus the required 129, independently confirming the source-level cause. The consumer demand correction and complementary early-stop/error test pass locally: all 1,741 non-ignored Execution tests pass, with seven pre-existing ignored cases. Complete collection now uses exactly 129 calls; both sensitive consumers stop production at three rows. The normal controlled main follow-up remains required; lower operation counts alone do not qualify latency.

## Ownership and preservation argument

Execution owns consumer demand and physical scheduling. Core values, SQL name/type analysis, Storage interfaces and Engine session/transaction adapters need no new algorithms. The consumer interface defaults to one-row demand, preserving every existing custom or early-stop consumer. Only a consumer that accepts the complete input may request the producer's ordinary bounded batches. The existing directional path remains row-at-a-time, and LIMIT, scalar-cardinality, routine and set-input boundaries retain their own demand.

Let an input produce ordered rows R and a row-sensitive consumer stop or fail after prefix R[0:k]. Its default one-row request evaluates the same prefix, preserves the first reached failure and closes through the existing cleanup path. For the complete public-result consumer, every successful delivery returns Continue and its result is the ordered concatenation of all row observations. Partitioning R into the producer's native bounded batches instead of singleton batches changes no value, type, duplicate position, name, ordering, row count or evaluation multiplicity. Each physical batch is consumed and released before the next pull; the final API still owns O(N) result rows. The native producer continues to enforce workspace and cancellation boundaries. No new algebraic operator, approximation or optimizer rewrite is introduced.

## Historical evidence

#653 retains older inconclusive reference calibration, collected hardware/resource observations, source-provenance corrections and previous materialized-result investigations. A new accepted source-specific result must not erase those records, assert a retrospective hardware cause without evidence, widen a limit or turn an inconclusive interval into acceptance. The original records remain distinct from this live delivery state.

## Verification

Use the owning Execution test target for producer-call bounds, exact output, shared-owner release, row-level stop/error behavior and late cleanup. Reuse the existing independent PostgreSQL projection/consumer fixtures for public API coverage; preserve one test executable per crate. Finish source review, formatting, ownership/dependency and regression inventory checks before the draft becomes ready. Read actual Linux/macOS work/memory/output and affected FDW results for the final source. A normal main merge triggers the controlled runner; do not manually dispatch, retry or cancel measurements. Keep the reference executable, fixture, independent calibration and acceptance limits unchanged.

## Local correction evidence

The pre-correction test at `f4576f245` fails with 16,385 producer calls versus the required 129. The corrected Execution suite passes 1,741 tests and preserves its seven existing ignored tests. The exact early-stop/error case passes with three produced rows, three pulls, one close and released physical owners. Regression inventory validation covers 1,733 checks / 2,435 required cases. Workspace dependencies (110 runtime edges / 34 crates), Engine capabilities (3,204 files / 124 adapters), integration harness registration (230 sources / 23 targets) and the 1,000-line Rust ceiling pass. Public PostgreSQL fixture acceptance and strict Clippy are still pending; timing acceptance is not inferred.
