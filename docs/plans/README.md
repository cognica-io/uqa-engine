# Implementation plans

Files in this directory record staged implementation work. A living plan must describe the current repository boundary and remaining work, while a completed plan remains historical evidence unless its contract itself changes.

| Plan | Lifecycle | Update rule |
| --- | --- | --- |
| [`0001-uqa-engine-implementation-plan.md`](0001-uqa-engine-implementation-plan.md) | Living | Update when workspace ownership, public surfaces, engineering policy, verification, or release gates change. |
| [`0002-benchmark-coverage.md`](0002-benchmark-coverage.md) | Complete | Update only when the completed benchmark-coverage contract or its evidence changes. |
| [`0003-postgresql-18-compatibility.md`](0003-postgresql-18-compatibility.md) | Active | Update in every PostgreSQL 18 compatibility PR that changes a manifest item, milestone, supported surface, or remaining gate. |
| [`0004-mlx-runtime-support.md`](0004-mlx-runtime-support.md) | Active | Update whenever the model format, backend contract, native-runtime lock, platform or package matrix, rollout phase, or release evidence changes. |
| [`0005-rust-workspace-refactoring.md`](0005-rust-workspace-refactoring.md) | Complete | Update only when the completed ownership contract, permanent line policy, test topology, or final evidence changes. |
| [`0006-nori-analyzer.md`](0006-nori-analyzer.md) | Active | Update whenever Nori contracts, implementation tasks, dictionary/reference inputs, integration behavior, or verification evidence changes. |
| [`0007-kuromoji-analyzer.md`](0007-kuromoji-analyzer.md) | Active | Update with each logical Kuromoji implementation unit that changes ownership, interfaces, reference inputs, completion status, verification evidence, or remaining acceptance gates. |
| [`0008-concurrent-storage-transactions.md`](0008-concurrent-storage-transactions.md) | Active | Update with each concurrent-storage implementation unit that changes interfaces, ownership, provider coverage, migration, isolation or acceptance evidence; common records, private changes, snapshot reads and redb record persistence are implemented, while SQLite, legacy store routing and SQL integration remain pending. |
| [`0009-nori-index-retention.md`](0009-nori-index-retention.md) | Complete | Preserve the unchanged allocation gates, fixed owner contracts and full CI evidence merged in PR #149. |
| [`0010-sqlite-index-snapshots.md`](0010-sqlite-index-snapshots.md) | Complete | Preserve fixed unbound SQLite snapshots, retained ownership and final platform acceptance recorded in PR #155. |
| [`0011-committed-notification-recovery.md`](0011-committed-notification-recovery.md) | Complete | Preserve the atomic publication, recovery, retained completion and final platform acceptance recorded in PR #154. |
| [`0012-compressed-container-read-consistency.md`](0012-compressed-container-read-consistency.md) | Complete | Preserve authenticated file/map ownership and deterministic compaction regressions verified in PR #154. |
| [`0013-added-column-publication.md`](0013-added-column-publication.md) | Active | Track declared-column and physical-field publication order, PostgreSQL outcomes, provider rollback/reopen acceptance and final review. |

The PostgreSQL 18 plan contains a compact ledger generated from `tests/parity/pg18/manifest.json`. `python3 tests/parity/pg18/run_diff.py --validate-manifest` rejects any manifest change whose plan ledger was not updated in the same change, so the readable plan and machine-readable accounting cannot silently diverge again.

Record a newly confirmed gap in its active plan and evidence manifest as incomplete when implementation begins; change it to complete or verified only after the documented exit evidence passes. Do not leave active work visible only in a branch name, issue, or conversation.
