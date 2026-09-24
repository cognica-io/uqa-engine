# Committed notification publication recovery

Status: Active; three-provider loss reproduced and atomic publication storage verified; registry recovery and Engine integration remain pending.

Issue: [#129](https://github.com/cognica-io/uqa-engine/issues/129). Initial source: merged main `b2a94f3c`. Work branch: `fix/committed-notification-recovery`. PR #151 has merged; no additional PR opens while the next correction, PR #152, is active.

## Reproduced boundary

`Engine::begin_notification_commit` prepares changes in a separate SQLite notification registry transaction. `commit_transaction_frame` then commits the main storage backend, and `finalize_cross_commit` commits the registry afterwards. Dropping the unfinished registry guard rolls back its publication even when the main records are already committed. No surviving session can reconstruct the omitted payload from the main store.

The deterministic `notifications::recovery_tests::committed_notifications_survive_sender_loss_before_queue_publication` regression creates a live listener, writes a private row and notification, verifies their invisibility, prepares the actual notification guard, commits the actual backend, and drops the guard and sender. Native SQLite, SQLite Key/Value and redb each retain row `1` but deliver zero notifications instead of one. All three cases fail at the delivery assertion in Linux Docker; no sleeps, timing comparisons or probabilistic fault injection establish this result. The regression also requires the original sender identifier and payload and no duplicate on a second poll once the correction is implemented. Raw build/test output stays outside Git.

## External contract

PostgreSQL 18 [async.c](https://github.com/postgres/postgres/blob/REL_18_STABLE/src/backend/commands/async.c) places pending notifications in its queue before recording transaction commit, then updates local subscriptions and sends wake signals after commit. Readers inspect transaction completion before delivery. Its queue is not a database-crash durability guarantee. This correction covers a committed sender disappearing while listeners survive; it does not promise exactly-once client acknowledgement or replay notifications to newly created listeners after all prior listeners have disappeared.

Preserve transactional LISTEN/UNLISTEN, outer-transaction delivery, savepoint rollback, per-transaction channel/payload deduplication, original sender identity, commit order, queue capacity and delayed delivery to listeners with open transactions. LISTEN and NOTIFY must remain legal in read-only SQL transactions. A failed post-commit publication must never be reported or treated as if the already committed data were rolled back.

## Ownership and existing interfaces

The relevant manifests and `scripts/workspace-dependency-policy.json` were inspected before selecting interfaces. Common Storage depends on Core and Analysis and cannot depend on SQL, Execution or a physical provider. SQLite owns physical SQLite transactions and encryption; redb depends on common Storage and must not acquire a SQLite dependency. Engine already constructs its shared notification hub and owns session subscription state, pending transaction effects and completion adapters. Keep notification persistence, recovery, encoding and reclamation algorithms in their storage owners.

`CatalogFacade` provides transactional metadata reads/writes, while `PersistentStorageBackend` owns begin/commit/rollback, snapshot refresh and transaction outcome classification. `VersionedPersistence` atomically commits prepared records with durable receipts; the receipts already have bounded ownership and reclamation rules. `PreparedRecordCommit` and `StorageReadControl` provide admitted immutable buffers and cancellation. Reuse these capabilities where their semantics fit, without routing algorithms back through Engine or changing dependency direction.

An ordinary catalog metadata write is insufficient by itself: it can turn a legal read-only NOTIFY transaction into a storage write, follow an old session snapshot during recovery, or leave unbounded acknowledged records. Select a typed auxiliary publication contract after inspecting both physical commit implementations, preserving logical read-only status and the existing outcome-resolution owner. Do not enlarge or retain every ordinary transaction receipt merely to implement notifications.

The inspected `VersionedSession::commit_transaction` acknowledges completion before releasing its active transaction; managed receipt owners therefore cannot serve as an indefinitely recoverable notification reference. A missing reclaimed receipt is not proof of rollback. Publication must retain its own authoritative committed record until the queue acknowledgement allows reclamation.

## Selected publication record

Use one bounded pending-publication slot per notification registry. The existing registry transaction serializes preparation through the main commit; recovery must finish a prior committed slot before another sender prepares its slot. A fixed record avoids leaving one permanent MVCC head tombstone per historical notification transaction. Retained versions continue to obey the provider's existing snapshot and reclamation contracts.

Common Storage owns an immutable intent containing registry incarnation, sequence/position boundaries, sender identity and original ordered payloads. Stage it as an explicit auxiliary transaction effect, separately from user-record mutation accounting, so legal read-only SQL remains read-only. Materialize the slot under the existing current-snapshot/atomic-commit mechanism; reuse controlled record buffers and native/Key/Value record layouts so provider encryption and conditional publication remain effective. Ordinary catalog `set_metadata` calls from Engine must not bypass the storage read-only checks.

The registry commits its message publication and an acknowledgement identity together. If the sender disappears before that commit, a surviving listener reconstructs the messages from the main-store intent. If it disappears after that commit but before slot cleanup, the acknowledgement prevents duplicate append. Clear only the matching intent through fresh autonomous storage state, and keep acknowledgement storage bounded. A registry incarnation mismatch or incompatible format must fail explicitly. Migration must also fence already-open incompatible registry users.

Audit receiver cursors at the same boundary: a sender-owned deferred in-memory delivery must not permanently advance another live listener's durable cursor before that listener can receive or recover the message. Preserve the distinction between publication recovery and an application acknowledging a drained client notification.

## Required storage contract

- Retain an immutable, bounded publication intent with the sender identity, original ordered notifications and the identity needed for idempotent recovery. The intent becomes committed atomically with the authoritative data outcome. A transaction with notifications but no data writes follows the same completion contract without becoming an SQL write.
- Validate and reserve queue capacity before the main commit. Serialize queue admission, subscription changes and recovery so later senders cannot reuse an unresolved sequence range or overtake earlier committed notifications.
- Recover from a fresh committed storage view, including while an unrelated listener retains an older SQL snapshot. Never rerun SQL, callbacks, triggers or payload evaluation to reconstruct an intent.
- Apply publication and its acknowledgement atomically within the registry. A crash before or after that point must neither lose a committed event nor append it twice. Persist enough identity until the authoritative acknowledgement permits bounded intent reclamation.
- Preserve alive-listener cursors and subscription commit order. Polling and the existing wake worker must recover pending publication; correctness cannot depend on the failed sender delivering a wake signal.
- Preserve encrypted auxiliary files, admitted memory, cancellation before commit, and uncancellable cleanup/outcome resolution after commit. Validate format changes and fence incompatible readers if new persistent records require them.

## Implementation and acceptance

1. Keep the actual three-provider failure as the first regression. Add owner tests for the selected storage intent, publication identity, bounded decoding and cleanup contract.
2. Implement the common-storage intent and outcome lifecycle, then atomic SQLite/redb persistence under their existing commit boundaries. Cover notifications without data writes, aborted and indeterminate outcomes, acknowledgement and final reclamation.
3. Move or extend registry persistence in the SQLite owner and expose a narrow typed interface to the Engine session adapter. Retain existing queue limits and encryption. Add deterministic recovery ordering and duplicate-publication tests.
4. Connect Engine preparation, committed completion and listener polling/wake adapters to the storage contract. Keep authoritative committed outcomes even if a later registry operation fails.
5. Verify ordinary/explicit/read-only transactions, savepoints, rollback, subscription changes, duplicate payloads, queue-full rejection, sender loss at every commit boundary, retained listeners, provider reopen, encrypted files and independent-process recovery. Verify no callback replay and no delivery to a listener that subscribed only after the original commit.
6. Run the affected owner/public regressions, strict Clippy and dependency/ownership/harness checks. Update the manual, HISTORY, PG18 manifest and this plan with actual passing evidence. Open the correction PR only after the preceding PR is merged.

## Current acceptance

The source reproduction is executed and fails in the expected place for all three providers. Shared notification payloads, page alignment and queue-capacity calculations now live in `uqa-storage::notifications`; Engine imports that owner instead of maintaining its own implementation. The unchanged layout regression moved with the algorithm. That owner test, the Engine warning test and all three registry owner tests pass in Linux Docker. Strict all-target Storage/SQL/Execution/Engine Clippy and dependency/ownership/harness checks pass for this extraction.

The common-storage immutable publication codec now retains one shared admitted buffer and decodes borrowed payloads without a second message allocation. Its versioned UTF-8 framing records the registry incarnation, original sender and sequence/page boundaries; a fingerprint distinguishes different payloads under the same publication identity. All nine notification-owner checks pass in Linux Docker, including independently written bytes and page expectations, every truncated prefix, malformed/overflowed fields, maximum byte lengths, cancellation, quota cleanup and clone lifetime. Strict all-target Storage/SQL/Execution/Engine Clippy and the ownership, dependency, harness, header and file-limit checks also pass. The codec feeds the typed atomic publication contract below.

The typed notification publication capability now stages one immutable auxiliary effect in the existing common MVCC transaction. It preserves logical read-only accounting, savepoint rollback, sealed payloads and receipt-based retries; materialization uses the latest committed slot revision and refuses to overwrite an unacknowledged intent. Fresh reads bypass a listener’s pinned SQL snapshot. Autonomous acknowledgement clears only the matching fingerprint and preserves the caller’s active SQL transaction. Native SQLite owns its metadata row layout; SQLite Key/Value and redb reuse common metadata addressing. No dependency direction changes or Engine algorithms are introduced.

Linux Docker passes all 173 common MVCC session regressions and 514 common Storage unit tests, including read-only SSI/deferrable transactions, retained readers, cancellation, lost commit replies and stale acknowledgement. The shared provider schedule passes native SQLite and SQLite Key/Value in plain, encrypted, compressed and compressed-encrypted modes, plus redb: data and intent commit together, aborted/savepoint intents disappear, old query snapshots retain old data while recovery sees the new intent, original payloads survive closing every handle, and acknowledgement survives reopen without committing the caller’s private work. Encrypted main files contain no plaintext fixture payload. Strict all-target Storage/SQLite/redb Clippy and repository policies pass.

Registry recovery, passing sender-loss regressions, independent-process acceptance, full reclamation and format-fencing checks, final encryption acceptance and review remain. Existing successful notification tests establish ordinary behavior only and do not close the recovered-publication requirement.
