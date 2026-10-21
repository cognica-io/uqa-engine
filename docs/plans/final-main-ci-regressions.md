# Repair final-main CI regressions

The maintainer requested that the current work be fixed, verified and merged before pausing. Monitoring alone does not complete that request. This delivery repairs the confirmed failures on `38f41167e` in one PR, with separate logical implementation commits. The completed #1216–#1221 changes and the original remaining obligations in #714, #1108, #1117 and #653 remain preserved.

## Scope and current evidence

| Task | Confirmed failure | Owner and completion condition | Status |
| --- | --- | --- | --- |
| [#1223](https://github.com/cognica-io/uqa-engine/issues/1223) | Nori redb peak 8,292,358 bytes exceeds the unchanged 8,292,342-byte ceiling | Storage private-root representation; retain allocation, output, fork, savepoint and spill contracts | Fixed; unchanged local and main allocation/output/reopen gates pass |
| [#1224](https://github.com/cognica-io/uqa-engine/issues/1224) | Existing repeatable-read test exceeds 300 seconds on Linux and macOS | Storage/Execution path responsible for private-write work; preserve the fixture, allowance and timeout | Ordered merge and scoped reads pass the original three-provider test locally; automatic OS checks pending |
| [#653](https://github.com/cognica-io/uqa-engine/issues/653) | Controlled Q1 +11.4%, materialized scan +19.5%, cursor scan +17.4% | Owning execution or storage path; pass the same independently calibrated controlled comparison | Scan/grouping fixes pass 136 local cases; controlled qualification pending |

The failing workspace run is [39542443925](https://github.com/cognica-io/uqa-engine/actions/runs/39542443925). The signed controlled run [39542443736](https://github.com/cognica-io/uqa-engine/actions/runs/39542443736) retains 17 accepted comparisons and three regressions, with no inconclusive intervals. Its fixed reference is `0e8be6892340b2d1892e0abf68c2a9ad046f49d5`; these ratios are not PostgreSQL comparisons. Raw reports and traces remain outside version control.

## Delivery checklist

- [x] Correct private-root metadata and pass the unchanged Nori allocation/output gate.
- [x] Correct the repeatable-read slow path and verify all three providers locally with deterministic work coverage.
- [ ] Correct Q1 and result-scan repeated work and pass controlled qualification without widening limits or discarding observations.
- [x] Run formatting, strict owner Clippy, 289 owning-crate cases, dependency/ownership checks and complete-diff review.
- [x] Verify the final product source in automatic Linux/macOS work, memory and output regression checks.
- [ ] Verify the affected full workspace tests on merged main.
- [x] Verify the final-ready FDW selector against the exact product tree; no foreign-handler input changed and no skipped matrix is counted as a test pass.
- [x] Merge PR #1225 as `916fb1159`, close #1223/#1224, remove its local/remote branch and update main with the unrelated parity README edit preserved.
- [ ] Complete remaining qualification, resolve any failures, synchronize these records and then pause.

The existing `tests/parity/pg18/README.md` edit in the main checkout is unrelated and remains untouched. Reuse the established local build checkout and target cache; use local correctness/work diagnostics while CI supplies controlled timing. Repository events trigger CI; no workflow dispatch, rerun or cancellation is part of this repair.

## Repair evidence

The private-root representation now stores provider revision summaries and an explicit early spill threshold as mutually exclusive policies, removing the redundant optional threshold from normal roots. Fifty local Storage overlay tests pass, including frozen revisions, failed admission, nested savepoints and explicit spill-policy preservation through fork and rollback. One allocation-only run passes all four unchanged redb Nori ceilings and live/reopened outputs. `commit_batch_256/0` peaks at 8,292,342 bytes, exactly the existing limit; no timing was compared.

The original repeatable-read test also times out locally at its existing 300-second limit. One stack sample attributes the active path to repeated encrypted identity-block reads from retained document projections and visible-row filtering. Execution now holds Storage's existing bounded read scopes across those batches; focused owner and integration verification is in progress. No fixture, allowance or timeout changed.

The deeper repeatable-read cause is quadratic identity filtering: each page first searches for enough unchanged base IDs, scanning the whole remaining base when private changes replace every row. The corrected cursor merges base IDs and private changes in order and stops when the requested output page is full. A deterministic regression requires exactly one base visit per returned replacement over the complete scan. Identity-only projections also perform zero payload reads. The first complete merge-path check passed 102 focused Execution cases and exposed one captured-cancellation precedence failure, subsequently corrected and verified before output allocation. The original three-provider integration case passes locally in 248.289 seconds with CI's `opt-level=2`, the original 300-second timeout, 20,000-row fixture and 4MiB allowance. The unoptimized diagnostic runs still timed out and remain recorded as failures. This local correctness result is not a controlled latency comparison.

Analytical repair candidates remove per-row adaptive page division and use the largest observed row to prevent repeated prefetch of mixed-width tails. The existing exact batch-size bound remains enforced for every row. Compact text-pair grouping now binds the exact two slots and packs zero-to-three bytes directly, preserving the prior encoded keys, null distinction and UTF-8 bytes. All 136 local aggregation, DISTINCT, scored-input and sizing cases pass, including a new mixed-width visit-count check. Controlled timing is still required; no local timing is claimed as acceptance.

The final combined local owner run passes all 289 Storage/Execution cases, including the corrected captured-cancellation precedence, all four new work/memory checks, spill-policy rollback and exact grouping keys. The checked-in FDW selector finds no affected foreign-handler input locally; that exclusion is not counted as an executed test pass. Build outputs remain in the shared cache (57.27GB, below the existing 100GB limit).

## Automatic qualification of the merged repair

PR run [39561493248](https://github.com/cognica-io/uqa-engine/actions/runs/39561493248) verifies the exact `a55a0aebb` product tree, which equals merged main `916fb1159`: both Linux and macOS pass 1,536 checks / 2,127 cases and eight independent-process histories, with 676 registered source hashes and exact JUnit identities checked and no failures, errors or skips. FDW scope [39561532911](https://github.com/cognica-io/uqa-engine/actions/runs/39561532911) finds no changed foreign-handler input.

Main Nori job [119113658822](https://github.com/cognica-io/uqa-engine/actions/runs/39567631928/job/119113658822) passes all five analysis/index/phrase/SQLite/redb report validators against unchanged limits and expected outputs. The redb seed-256 peak is 8,292,342 bytes; all four redb transactions release their retained allowance. Artifact verification repeats no measurement. Full workspace [39567631928](https://github.com/cognica-io/uqa-engine/actions/runs/39567631928), main work/memory/output checks [39567631962](https://github.com/cognica-io/uqa-engine/actions/runs/39567631962) and controlled EC2 [39567631944](https://github.com/cognica-io/uqa-engine/actions/runs/39567631944) remain in progress. The task is not paused or complete.
