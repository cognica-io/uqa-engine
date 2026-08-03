# UQA-RS design documents

The design directory records current architecture contracts, security boundaries, migration decisions, compatibility fixtures, and benchmark evidence. The repository README explains how to use UQA-RS; documents here explain why the implementation is shaped the way it is and which invariants future changes must preserve.

The research-level foundation is developed separately in [A Typed Carrier Algebra for Unified Query Execution](../papers/typed-carrier-algebra.md), whose relationship to the DOI-published source papers is stated in the manuscript and in the repository's [citation metadata](../../CITATION.cff).

## Start here

| Document | Scope | Primary audience |
| --- | --- | --- |
| [System architecture](architecture.md) | Crate layers, carrier contracts, unified planning, physical execution, storage, retrieval, graph, ML, and extension boundaries | Contributors and integrators |
| [Vector indexes](vector-indexes.md) | Brute-force, IVF, and HNSW selection, parameters, mutation, persistence, cache revisions, and search guarantees | Storage and retrieval contributors |
| [Engine state ownership](engine-state-ownership.md) | Session isolation, mutable state domains, lock ownership, epochs, transactions, and publication order | Engine and concurrency contributors |
| [Compressed VFS security](compressed-vfs-security.md) | Authenticated format, threat boundary, rollback protection, trusted anchors, and deployment choice | Security reviewers and operators |
| [Key/value storage migration](kv-storage-migration.md) | Logical key layout, backend abstraction, migration phases, compatibility, and performance criteria | Storage contributors |
| [Parity fixtures](parity.md) | SQL golden data, relevance fixtures, vector calibration gates, versioning, and CI use | Test and compatibility contributors |
| [Performance](performance.md) | Benchmark provenance, regression gates, measured bottlenecks, optimizations, and limitations | Performance contributors and evaluators |

## Reading paths

- To understand a query from SQL text to result batches, read [system architecture](architecture.md), then follow its links to planner and execution code.
- To add session, catalog, cache, or transaction state, read [engine state ownership](engine-state-ownership.md) before changing `Engine`.
- To select an encrypted storage mode, read [compressed VFS security](compressed-vfs-security.md) and prefer SQLCipher when compression is not required.
- To change a benchmark or make a performance claim, read [performance](performance.md) and preserve fixture, provenance, and ratio-gate comparability.
- To change a compatibility or calibration contract, read [parity fixtures](parity.md) and version the affected manifest.
- To change physical vector indexing, read [vector indexes](vector-indexes.md) and preserve its algorithm, transaction, and reopen invariants.

## Plans versus design contracts

Files under [`docs/plans/`](../plans/) describe staged implementation work and historical sequencing. Files in this directory describe the current contract; when implementation work changes a boundary, update the relevant design document in the same change.
