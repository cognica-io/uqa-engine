# DiskANN backup and history restoration

This acceptance boundary covers a closed, consistent provider-file copy containing a selected DiskANN generation and canonical changes not yet covered by that generation. It uses the existing database restoration APIs; it introduces no new backup API, record format, index algorithm or SQL behavior. Publication interruption, corruption rejection and platform/resource acceptance retain their separate obligations in the [implementation plan](../plans/0014-diskann-vector-index.md).

## Ownership and restoration boundary

Common Storage owns the shared [fixture and assertions](../../crates/uqa-storage/src/key_value/conformance/origins/binding/restore.rs), including real graph construction, exact-side vectors, tensor scoring, change accounting and later reconstruction. SQLite and redb own physical copies, history publication, crash/synchronization failures and reopen. Engine does not implement a restore or backup algorithm.

The caller closes every provider, session, retained query and snapshot before copying a file. SQLite copies the closed main file; the restored path initializes its own auxiliary coordinator. Both native SQLite and SQLite Key/Value preserve their original data mapping. redb exclusively opens the copied database. The caller keeps one `DatabaseRestore` request outside the database and uses the same request after an error. An ordinary reopen preserves history; explicit restoration creates the request's new history incarnation.

SQLite [restore publication](../../crates/uqa-storage-sqlite/src/mvcc/restore.rs) publishes a durable intent, restores the auxiliary coordinator, and then publishes the new main history identity with the intent cleared. An interrupted intent rejects ordinary opens until that original request completes. redb [restore publication](../../crates/uqa-storage-redb/src/mvcc/restore.rs) changes history and retires old receipt/SSI state in one physical transaction. Neither path rewrites canonical rows, vector origins, selected heads, manifests, PQ records, exact-side records, graph pages, complete-origin artifacts or uncovered journals.

## Preservation argument

Let $D$ map each physical logical-record key to its visible revision and optional value, including tombstones. Let $H$ be the transaction-history incarnation, $N$ the independent persistent data namespace, and $G$ the selected DiskANN generation in that namespace. Under a closed, consistent copy and a supported format, successful restoration has the form

$$
\operatorname{restore}_{H \to H'}(D,H,N,G)=(D,H',N,G),\qquad H'\ne H.
$$

The SQLite main transition updates only the history header and receipt table; its auxiliary transition replaces coordination state. The redb transition updates the corresponding header, receipt and coordination tables atomically. These locations are disjoint from the records represented by $D$. Native record addressing uses its retained data namespace, and common DiskANN addressing uses the persisted data identity. Thus changing $H$ changes neither the key used to select $G$ nor any reachable record's bytes or revision. A pending SQLite intent exposes no ordinary database handle; a failed redb synchronization can leave either complete history, and retrying the same request resolves which one without applying a second data transformation.

For a fixed query $q$ and fixed search configuration, the selected graph, PQ codes, complete origins, canonical tensors and visible change records therefore remain the same. The existing query algorithm receives the same coordinate bits and candidate identities, so it produces the same full scored result, not merely the same document support:

$$
\operatorname{Query}(D,N,G,q)=\operatorname{Query}(\operatorname{restore}(D),N,G,q).
$$

This argument adds no commutativity, idempotence or semiring law to payloads. Existing relational and ranked compositions consume unchanged values and scores. Approximate traversal retains its existing assumptions because restoration neither changes the graph nor substitutes another access method. Missing or corrupt index state is not repaired by this operation.

Origins from $H$ remain immutable data after old receipts are retired. New mutations use $H'$, so their transaction/revision pairs cannot alias an origin from $H$, even if allocation numbers coincide. Data namespace and generation watermarks remain unchanged; subsequent reconstruction must reserve a strictly newer generation while preserving table/index mappings. Retained queries opened after restoration keep their captured old generation and changes through that reconstruction. A completed retry of the original restore request sees $H'$ and leaves subsequent writes, receipts and the newer generation intact.

## Executable evidence

The fixture builds actual graph/PQ/page/origin artifacts from axis vectors, a two-vector tensor and a zero vector handled by the exact-side path. It then updates one document, deletes another and inserts a new tensor without rebuilding. Literal scores, tensor counts, row values and exact outstanding document/vector/byte counts are checked before and after restoration. Later writes and a rebuild must coexist with old-history origins and a retained query view.

| Obligation | Owning regression |
| --- | --- |
| Closed-file copy, new history, unchanged selected generation and all visible record revisions/bytes | SQLite `mvcc::restore::tests::diskann::diskann_backup_restore_preserves_records_changes_and_new_history_writes`; redb `mvcc::tests::diskann::restore::diskann_backup_restore_preserves_records_changes_and_new_history_writes` |
| Plain, encrypted, compressed and combined SQLite files, both native and Key/Value mappings | The SQLite regression runs all eight configurations; redb supplies the ninth persistent configuration |
| Literal query/row preservation, uncovered changes, new writes, retained old queries and strictly newer reconstruction | Shared Storage restore conformance called by each provider regression |
| Retrying the same restore request after new writes and reopening normally preserve those writes | Each provider compares the post-write record image and selected generation across both operations |
| Restoring the copy leaves the original history and its data unchanged | Each provider reopens the original and checks its history identity, complete record image and literal results |
| Process loss after SQLite intent or coordinator publication rejects ordinary opens and resumes the original request | SQLite `mvcc::restore::tests::diskann::diskann_backup_restore_resumes_after_process_loss_at_durable_boundaries`, in both mappings and all four file modes |
| Failed redb synchronization preserves the complete DiskANN image in either permitted history and converges on retry | redb `mvcc::tests::diskann::restore::diskann_backup_restore_resolves_failed_synchronization_without_losing_artifacts` |

Record-image checks page through every visible physical key, including tombstones, and compare revision, record count, sequence, reclamation epoch and a length-framed SHA-256 digest of keys and values. This compact fixture evidence does not retain machine reports or claim a formal collision-free hash. Literal query and row expectations are independent of the record digest. These are functional checks, with no timing, allocation or RSS acceptance claim.
