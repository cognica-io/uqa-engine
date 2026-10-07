# Exact row reservation identities

Execution owns cross-process row arbitration in `uqa-execution::row_locks`. The former 62-bit relation/document hash also acted as equality: unrelated rows could conflict when their hashes matched. Hashing must select a table probe, while the retained claim and wait edge compare the complete identity.

## Representation and ownership

Ordinary row claims use the exact relation registry's immutable generation together with the full 64-bit document identity. The registry compares relation bytes in full inside its existing encrypted SQLite sidecar. Holders and waiters pin the relation slot; its generation never changes while pinned. Each acquisition retains a pin, including an upgrade below a savepoint, and each release removes that acquisition's pin. The operating-system pin count follows distinct relations, not rows.

Key reservations already enter the lock manager as 32-byte identities. Their complete bytes and document identity fit directly in the shared entry, in a namespace distinct from registered relations. They do not add a registry write or a native relation pin per key. This preserves the key identity contract supplied by mutation execution; it does not claim that a digest is an injective encoding of arbitrary SQL values.

Shared entries encode the full identity, owning process generation, session, and key/row modes. A hash chooses the probe start only. Wait records retain that same complete identity so a descriptor collision cannot invent a wait-for edge. Batch ordering groups by exact identity before key/row mode, retaining duplicate and upgraded acquisition counts even when two descriptors have the same numeric address.

## Preservation argument

Let $R$ denote the exact relation bytes, $g(R)$ their registry generation while pinned, and $d$ the document identity. Simultaneously pinned distinct relations have distinct generations, because allocation increments one registry counter and slot reuse cannot change a live pin. Thus $(g(R_1),d_1)=(g(R_2),d_2)$ if and only if $R_1=R_2$ and $d_1=d_2$. Key identities inhabit a separate tagged namespace and compare all their supplied bytes.

For a claim identity $i$, the shared table retains the same holder sessions and key/row modes as before. The PostgreSQL tuple-lock conflict predicate is unchanged; only identities previously conflated by a hash collision become independent. Hash collisions still share a probe run, but no longer imply equality. Release, savepoint rollback, and wait traversal use the same identity representation, so neither a release nor a wait can affect a different row merely because its descriptor collides.

No relation, document, payload, score, or algebraic operator is added. Successful schedules preserve the original row observations and mutation boundaries. Removing false conflicts admits the independent schedules already permitted by PostgreSQL row identity semantics; it does not remove exclusion between conflicting modes on the same identity.

## Completion ledger

- [x] Inspect Execution's dependencies, features, relation registry, claim table, waits and ownership boundaries.
- [x] Implement complete claim identities and relation-scoped pin retention.
- [ ] Verify claim-table encoding, savepoints, process death, mapping fallback, full conflict matrix and deadlock edges.
- [ ] Verify collision separation and bounded registry/native-pin growth with actual acquisition paths.
- [ ] Define and verify coordination-format admission and upgrade boundaries.
- [ ] Run focused checks, review, update the manual and regression inventory, and merge the change.

The remaining performance work is tracked separately: #266 also requires multi-process contention qualification and exclusion of incompatible legacy coordinators; #347 covers autonomous sequence publication; #348 covers bundled SQLite's memory-management flag; #261 covers controlled timing and reviewed baselines; #125 covers macOS loader startup. This change must not close those unverified obligations.
