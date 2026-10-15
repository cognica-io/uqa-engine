# Composite comparison acceptance

This is the combined exit audit for original record 64 preserved in [PR #855](https://github.com/cognica-io/uqa-engine/pull/855) and the [current task map](0018-extension-progress.md). It resumes from released main `c84b2601ae5cb238a18071b6633c2b107f0d9c6f`. The complete original requirement includes composite creation, I/O, nested values, attribute changes, dependencies, record comparisons and every consuming execution path; the later range and extension-package units remain separate required work.

SQL owns declared and reached-field capabilities, record equality/order, catalog decoding and typed key semantics. Execution owns bounded relational and mutation consumers; Planner retains operator selection and expression identity. Core and Storage retain value carriers and provider boundaries. Engine supplies catalog, transaction and retention adapters. The current SQL/Execution/Engine manifests, enabled features and workspace dependency policy have been inspected. No new dependency or feature is planned for this audit.

## Requirements and evidence

The named files below are existing independently captured PostgreSQL references under `tests/parity/pg18`. Their existence alone does not close a row: inspect the actual cases, consumer selection and source-matched execution evidence. Public replay must cover memory, native SQLite, SQLite Key/Value and redb, with actual persistent reopening.

| Requirement | Existing reference families | Remaining audit |
| --- | --- | --- |
| Composite creation, I/O, fields, arrays and nesting | `composite_type`, `composite_constructor_lifecycle` | Reconcile declared identities and lifecycle evidence with the original exit conditions. |
| ADD/DROP/RENAME/ALTER ATTRIBUTE, dependencies and retained tuples | `composite_attribute_*`, `composite_retained_layout`, `composite_tuple_*`, `composite_nested_layout` | Verify live/dropped physical slots, prepared/cursor reads, rollback and reopening; preserve the approved #656 bounds contract. |
| Scalar equality/order laws and NULL boundaries | `composite_key_semantics`, `composite_expression_comparisons` | Inspect field-family coverage, named/anonymous selection, reached errors, reflexivity, symmetry and transitivity. |
| Array consumers and declared capabilities | `array_record_comparisons`, `composite_comparison_capabilities` | Check early exits, unsupported field operators, domains, nested containers and empty inputs. |
| Hash/equality agreement, DISTINCT, grouping and set operations | `composite_key_semantics`, `composite_comparison_capabilities`, `composite_enum_set` | Verify equivalent physical representations and retained descriptors across in-memory and forced-spill paths. |
| Sorting, ordered aggregates and window peers | `composite_key_semantics`, `composite_enum_sort`, `composite_enum_aggregate`, `composite_enum_window` | Verify field operators, ties, NULL ordering, enum state, descriptors and bounded spill. |
| Direct/encoded/spilled joins and outer multiplicities | `composite_join_semantics` | Verify actual strategy coverage, SQL equality, reached diagnostics and retained enum identities. |
| Index probes, uniqueness, FK actions and command overlays | `composite_key_semantics` plus owner tests | Verify the same equality/order classes, representation-changing writes and persistent reopening. |
| Retained physical/enum consumers | `composite_datum_consumers`, `composite_catalog_array`, `composite_enum_*` | Reconcile shared versus private comparison state and lazy-read behavior with every consumer above. |

## Cross-consumer field reference

The independent `composite_consumer_matrix_oracle` captures 169 PostgreSQL 18.4 observations for double precision, numeric, bpchar, timetz, interval, JSONB, bytea, varying bits, integer arrays, int4range, int4multirange and oidvector fields. Each family uses the same retained composite values for scalar truth tables, comparison laws, DISTINCT, GROUP BY, window peers, joins, INTERSECT, B-tree probes and unique checks. The twelve final observations use a fresh PostgreSQL connection and actual provider reopening. Version, collector/input hashes and container identity are retained in the compact fixture provenance. Current-source replay is in progress; this is reference coverage, not passing Engine evidence.

## Progress

- [x] Confirm PR #855 is merged, its delivered issues are closed and current main includes the catalog and release corrections.
- [x] Preserve the full original task scope and identify the owning crates and existing independent references.
- [ ] Complete the coverage audit and capture independent PostgreSQL evidence for missing observations.
- [ ] Fix every reproduced difference in its owning crate, with focused regressions and a preservation argument.
- [ ] Verify the combined matrix on all providers, reopening and bounded consumer paths at the delivered source.
- [ ] Verify actual automatic Linux/macOS performance artifacts, synchronize public contracts and task status, review and merge the bounded follow-up PRs, and remove their branches.

Do not mark the broader composite unit complete from a single fixture, green archive job or closed prerequisite issue. Retain compact fixture provenance and source-matched evidence here; generated machine reports remain outside Git.
