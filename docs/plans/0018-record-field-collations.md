# Record field collation preservation

This unit fixes [#958](https://github.com/cognica-io/uqa-engine/issues/958) while original #638 record 65 continues after verified and merged [PR #957](https://github.com/cognica-io/uqa-engine/pull/957). An anonymous ROW currently loses its fields' selected collations when it becomes a shared value. The reproduced SQL test retains text OID 25 but loses collation OIDs 950 and 963. This unit preserves that metadata; complete selected comparison invocation remains original record 65 work.

## Owners and representation

Core carries an optional opaque `collation_oid` in `RecordFieldType`. `Some(0)` retains a selected indeterminate or noncollatable identity, while `None` preserves the absence of older metadata. The existing controlled constructors, retained-size accounting and copying own the descriptor allocation. Ordinary and controlled JSON readers accept the older two-field descriptor and validate an optional unsigned 32-bit collation identity.

SQL reuses `ScalarTypeSchema`'s existing selected column and expression metadata for ROW production. `RowSchema` and call-input views share that observation; input functions, domain checks and field expressions are not evaluated again to discover collation. Named record observation reads the pinned `CompositeTypeDescriptor`; anonymous observation reads the producer descriptor without accessing its field values. The original and invoking cancellation scopes remain attached to controlled production.

Execution's binary codec uses an explicit typed-row tag for descriptors that can carry collation metadata, including a presence flag for each field. It continues to read the historical untyped and OID/typmod-only row tags. Exact sizing includes those flags and present identities. Forced-spill tests must inspect the restored descriptor and every truncated encoded prefix; legacy-byte tests are constructed independently of the current encoder.

## Preservation argument

For a typed row, write its descriptor as \(D=((t_i,m_i,c_i))_{i=1}^{n}\), with each \(c_i\) an optional unsigned identity. The representation remains a product \((V,D)\) with \(|V|=|D|\); the existing width validation precedes ownership transfer. Erasing the new component yields \(\pi(D)=((t_i,m_i))_{i=1}^{n}\), so existing field values, OIDs and modifiers are unchanged. Core's value equality does not interpret these identities; representation equality distinguishes them.

The new codec writes the optional identity as a presence byte followed by four bytes when present. That representation is injective over `None` and every `Some(u32)`, including zero. Consequently \(\operatorname{decode}(\operatorname{encode}(V,D))=(V,D)\). Legacy decoding maps each absent identity to `None`, without guessing that a lost explicit selection was zero. Default resolution for absent legacy metadata remains with SQL's type/catalog owner.

Descriptor leases use the actual \(\operatorname{sizeof}(\texttt{RecordFieldType})\) and retained capacity. Constructors transfer admitted buffers; copies and tagged decoders reserve their actual descriptor storage before allocating it. A rejected width, exhausted allowance or either cancelled scope drops all temporary owners. This argument establishes metadata preservation and resource ownership, not selected collation ordering or comparator/index equivalence.

## PostgreSQL reference and qualification

An independent PostgreSQL 18.4 Docker capture contains 22 record comparison cases (SHA-256 `11967d04d6c8df98268698e51cf342725e54ffef8f81c455ee9eef08cbd7b93b`). Ten cases report exact `42P22` string-comparison diagnostics when corresponding field collations differ. Equal text still raises the error; a deciding earlier field, NULL comparison or empty array suppresses a later string comparison. Named and anonymous records use the same rule. PostgreSQL's [record comparison implementation](https://github.com/postgres/postgres/blob/REL_18_STABLE/src/backend/utils/adt/rowtypes.c) selects each reached field's descriptor collation and passes an invalid selection if the two descriptors disagree. These observations constrain the subsequent selected comparator; metadata round trips alone do not verify that comparator.

The new SQL regression fails before the correction because the constructed row has no collation metadata. Final focused validation passes nine Core row tests, 95 SQL collation tests, 37 Execution spill tests and four ordinary/admitted scalar collation tests. Strict Core/SQL/Execution library/test lint, rustfmt, the 109-edge dependency policy, Engine ownership, test-harness, header and source-size policies pass. The deterministic inventory now selects 864 checks / 1,263 cases. Automatic Linux/macOS work, memory, output and eight independent-process histories per OS remain required before delivery; no timing acceptance is claimed. All 80 original task records remain preserved and original record 65 stays open.
