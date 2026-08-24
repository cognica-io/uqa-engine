# PostgreSQL 18.4 MERGE and RETURNING oracle

This transcript was recorded on 2026-08-24 from `postgres@sha256:22c89fe0d0f507606260237fd55e51f6137f58b2d5bcf6152242b96d9fe8f9a4`, reporting `PostgreSQL 18.4 (Debian 18.4-1.pgdg13+1)` and `server_version_num = 180004` on `aarch64-unknown-linux-gnu`; the image architecture and OS were `arm64` and `linux`.

The oracle container was created with `docker run -d --name uqa-pg18-dml-oracle -e POSTGRES_PASSWORD=uqa_oracle postgres@sha256:22c89fe0d0f507606260237fd55e51f6137f58b2d5bcf6152242b96d9fe8f9a4`, and every error case used `psql -X -U postgres -v ON_ERROR_STOP=1 -v VERBOSITY=verbose` so the recorded SQLSTATE came from the server diagnostic.

## Candidate kinds, action order, and row images

The target contained `(1,10,'matched')`, `(2,20,'keep')`, and `(3,30,'remove')`; the source contained `(1,5,'m')` and `(4,40,'new')`. The clauses were deliberately interleaved as `NOT MATCHED BY SOURCE` conditional UPDATE, MATCHED UPDATE, `NOT MATCHED BY TARGET` INSERT, and unconditional `NOT MATCHED BY SOURCE` DELETE.

```sql
MERGE INTO target AS t USING source AS s ON t.id = s.id
WHEN NOT MATCHED BY SOURCE AND t.id = 2 THEN UPDATE SET val = t.val + 1, note = 'source-missing-updated'
WHEN MATCHED THEN UPDATE SET val = t.val + s.delta, note = s.marker
WHEN NOT MATCHED BY TARGET THEN INSERT (id, val, note) VALUES (s.id, s.delta, s.marker)
WHEN NOT MATCHED BY SOURCE THEN DELETE
RETURNING WITH (OLD AS before, NEW AS after) merge_action() AS action, s.id AS source_id, before.id AS old_id, before.val AS old_val, before.note AS old_note, after.id AS new_id, after.val AS new_val, after.note AS new_note, t.id AS target_id, t.val AS target_val;
```

| action | source_id | old_id | old_val | old_note | new_id | new_val | new_note | target_id | target_val |
| --- | ---: | ---: | ---: | --- | ---: | ---: | --- | ---: | ---: |
| UPDATE | 1 | 1 | 10 | matched | 1 | 15 | m | 1 | 15 |
| UPDATE | NULL | 2 | 20 | keep | 2 | 21 | source-missing-updated | 2 | 21 |
| DELETE | NULL | 3 | 30 | remove | NULL | NULL | NULL | 3 | 30 |
| INSERT | 4 | NULL | NULL | NULL | 4 | 40 | new | 4 | 40 |

The command tag was `MERGE 4`. A second oracle with MATCHED, `NOT MATCHED BY SOURCE`, and `NOT MATCHED BY TARGET` all selecting `DO NOTHING` returned zero rows and command tag `MERGE 0`, leaving the target unchanged.

## Visibility and reachability errors

Both `WHEN NOT MATCHED BY SOURCE AND s.id IS NULL THEN DELETE` and `WHEN NOT MATCHED BY SOURCE THEN UPDATE SET val = s.delta` failed with SQLSTATE `42P01`, because the source relation is not visible in that candidate kind. Both `WHEN NOT MATCHED BY TARGET AND t.id IS NULL THEN DO NOTHING` and `WHEN NOT MATCHED BY TARGET THEN INSERT (id, val) VALUES (t.id, t.val)` failed with SQLSTATE `42P01`, because the target relation is not visible in that candidate kind.

`ON 1` and `WHEN NOT MATCHED BY SOURCE AND 1` failed with SQLSTATE `42804`, because MERGE join and action conditions require boolean expressions.

A MERGE containing both source-missing and target-missing clauses with `ON t.id > s.id` failed with SQLSTATE `0A000` and `FULL JOIN is only supported with merge-joinable or hash-joinable join conditions`.

An unconditional `WHEN NOT MATCHED BY SOURCE THEN DO NOTHING` followed later by another `WHEN NOT MATCHED BY SOURCE` clause failed with SQLSTATE `42601` and `unreachable WHEN clause specified after unconditional WHEN clause`; an unconditional clause of another candidate kind between them did not change that result.

## Join cardinality and RETURNING star order

One source row matching two distinct target rows updated both target rows and returned two UPDATE rows. Two source rows selecting UPDATE for the same target failed atomically with SQLSTATE `21000` and `MERGE command cannot affect row a second time`; a candidate selecting `DO NOTHING` did not count as a prior modification and did not cause the cardinality error.

For a MATCHED UPDATE and a `NOT MATCHED BY TARGET` INSERT, unqualified `RETURNING *` produced source columns before target columns with the header `id|delta|marker|id|val|note`; the two rows were `1|5|m|1|15|m` and `4|40|new|4|40|new`.

The corresponding `RETURNING WITH (OLD AS before, NEW AS after) s.*, before.*, after.*` preserved source, old-image, and new-image groups in that order; the INSERT row contained NULL in every `before.*` position and the UPDATE row retained both target images.
