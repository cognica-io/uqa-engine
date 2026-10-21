# Repair final-main CI regressions

The maintainer requested that the current work be fixed, verified and merged before pausing. Monitoring alone does not complete that request. This delivery repairs the confirmed failures on `38f41167e` in one PR, with separate logical implementation commits. The completed #1216–#1221 changes and the original remaining obligations in #714, #1108, #1117 and #653 remain preserved.

## Scope and current evidence

| Task | Confirmed failure | Owner and completion condition | Status |
| --- | --- | --- | --- |
| [#1223](https://github.com/cognica-io/uqa-engine/issues/1223) | Nori redb peak 8,292,358 bytes exceeds the unchanged 8,292,342-byte ceiling | Storage private-root representation; retain allocation, output, fork, savepoint and spill contracts | Fixed; unchanged local allocation/output/reopen gate passes |
| [#1224](https://github.com/cognica-io/uqa-engine/issues/1224) | Existing repeatable-read test exceeds 300 seconds on Linux and macOS | Storage/Execution path responsible for private-write work; preserve the fixture, allowance and timeout | Replaced repeated suffix scans with ordered merging; final integration verification running |
| [#653](https://github.com/cognica-io/uqa-engine/issues/653) | Controlled Q1 +11.4%, materialized scan +19.5%, cursor scan +17.4% | Owning execution or storage path; pass the same independently calibrated controlled comparison | Inspecting repeated work in the measured queries |

The failing workspace run is [39542443925](https://github.com/cognica-io/uqa-engine/actions/runs/39542443925). The signed controlled run [39542443736](https://github.com/cognica-io/uqa-engine/actions/runs/39542443736) retains 17 accepted comparisons and three regressions, with no inconclusive intervals. Its fixed reference is `0e8be6892340b2d1892e0abf68c2a9ad046f49d5`; these ratios are not PostgreSQL comparisons. Raw reports and traces remain outside version control.

## Delivery checklist

- [x] Correct private-root metadata and pass the unchanged Nori allocation/output gate.
- [ ] Correct the repeatable-read slow path and verify all three providers with deterministic work coverage.
- [ ] Correct Q1 and result-scan repeated work and pass controlled qualification without widening limits or discarding observations.
- [ ] Run formatting, owning-crate regressions, dependency and ownership checks; review the complete diff.
- [ ] Verify the final product source in automatic Linux/macOS work, memory and output regression checks and the affected full tests.
- [ ] Qualify affected FDW inputs once when the completed draft becomes ready, keeping every logical commit and PR task record current.
- [ ] Merge, close resolved scoped issues, remove the completed branch, restore main and then pause.

The existing `tests/parity/pg18/README.md` edit in the main checkout is unrelated and remains untouched. Reuse the established local build checkout and target cache; use local correctness/work diagnostics while CI supplies controlled timing. Repository events trigger CI; no workflow dispatch, rerun or cancellation is part of this repair.

## Repair evidence

The private-root representation now stores provider revision summaries and an explicit early spill threshold as mutually exclusive policies, removing the redundant optional threshold from normal roots. Fifty local Storage overlay tests pass, including frozen revisions, failed admission, nested savepoints and explicit spill-policy preservation through fork and rollback. One allocation-only run passes all four unchanged redb Nori ceilings and live/reopened outputs. `commit_batch_256/0` peaks at 8,292,342 bytes, exactly the existing limit; no timing was compared.

The original repeatable-read test also times out locally at its existing 300-second limit. One stack sample attributes the active path to repeated encrypted identity-block reads from retained document projections and visible-row filtering. Execution now holds Storage's existing bounded read scopes across those batches; focused owner and integration verification is in progress. No fixture, allowance or timeout changed.

The deeper repeatable-read cause is quadratic identity filtering: each page first searches for enough unchanged base IDs, scanning the whole remaining base when private changes replace every row. The corrected cursor merges base IDs and private changes in order and stops when the requested output page is full. A deterministic regression requires exactly one base visit per returned replacement over the complete scan. Identity-only projections also perform zero payload reads. All 103 focused Execution cases pass; the unchanged three-provider integration test is running. Earlier local diagnostic attempts remain recorded as timeouts, not passes.
