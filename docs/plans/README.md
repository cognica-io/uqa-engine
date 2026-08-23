# Implementation plans

Files in this directory record staged implementation work. A living plan must describe the current repository boundary and remaining work, while a completed plan remains historical evidence unless its contract itself changes.

| Plan | Lifecycle | Update rule |
| --- | --- | --- |
| [`0001-uqa-engine-implementation-plan.md`](0001-uqa-engine-implementation-plan.md) | Living | Update when workspace ownership, public surfaces, engineering policy, verification, or release gates change. |
| [`0002-benchmark-coverage.md`](0002-benchmark-coverage.md) | Complete | Update only when the completed benchmark-coverage contract or its evidence changes. |
| [`0003-postgresql-18-compatibility.md`](0003-postgresql-18-compatibility.md) | Active | Update in every PostgreSQL 18 compatibility PR that changes a manifest item, milestone, supported surface, or remaining gate. |

The PostgreSQL 18 plan contains a compact ledger generated from `tests/parity/pg18/manifest.json`. `python3 tests/parity/pg18/run_diff.py --validate-manifest` rejects any manifest change whose plan ledger was not updated in the same change, so the readable plan and machine-readable accounting cannot silently diverge again.

Record a newly confirmed gap in its active plan and evidence manifest as incomplete when implementation begins; change it to complete or verified only after the documented exit evidence passes. Do not leave active work visible only in a branch name, issue, or conversation.
