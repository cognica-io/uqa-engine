# Bounded notification output on a SQL session

## Definition and ownership

Let a committed notification be the tuple (sender process ID, channel UTF-8 bytes, payload UTF-8 bytes). The observed carrier is a finite ordered sequence of these tuples or an explicit terminal delivery error. SQL's existing listener state consists of its backend identity, committed channel set, transaction/savepoint stack and original hub cursor. The optional adapter replaces only the delivery inbox; it creates neither a second listener nor a second SQL session. Engine owns this retained-resource adapter and reuses the existing subscription inbox, memory reservations and native committed-delivery code. No planner, algebra, storage-format or query operator is added.

The adapter may be configured once under the session statement gate when the transaction stack, committed channel set and historical inbox are empty. Every bound is positive. The session's final channel set is validated before notification commit. Adapters close the session on a terminal queue/source error and use the checked atomic drain; unchecked legacy draining cannot represent failure.

## Order and transaction preservation

The existing hub supplies committed notification entries in cursor order and selects entries by membership in the current committed channel set. For each successful delivery, the reused inbox copies that same tuple without changing any component and appends it at the tail. Draining removes tuples from the head. By induction on append and drain operations, every successful observation is an order-preserving prefix of the original selected committed sequence. Empty delivery is the sequence identity, and concatenation of successful finite batches is associative because the same ordered tuples are appended and removed. No cross-publisher ordering or commutativity beyond the original hub is asserted.

The adapter uses the original session ID in commit, native registry lookup and rollback. The original transaction-open flag continues to defer delivery. Existing pending LISTEN actions and NOTIFY deduplication remain in the same transaction and savepoint frames. Native prepared batches are private until the existing registry transaction succeeds. Therefore enabling the adapter changes none of the commit, rollback, duplicate elimination, channel-selection or sender-identity observations of a successful bounded history. Default sessions retain their existing unbounded queue and execute the identical legacy branch.

## Resource and failure preservation

Each queue slot, copied string capacity and native prepared batch holds a reservation in one memory budget. Admission precedes the relevant copy and includes already retained and reserved work. Thus their sum never exceeds the configured byte bound, and retained plus reserved notifications never exceed the configured count. The native visitor bounds scanned registry entries separately and retains the original cursor progression; it does not materialize the unbounded registry into a delivery vector.

If admission or the source fails, the inbox retains its first terminal error and clears delivery rather than reporting a shorter successful history. Atomic checked drain tests that error under the same lock that removes tuples. Subsequent checked reads return the retained failure until the adapter closes the session; they never interpret discarded tuples as an empty successful delivery. A channel-limit rejection occurs before either data or listener commit, so it leaves the predecessor channel set unchanged. No timer establishes readiness, commit or recovery. Existing SQL operations and successful compositions remain identical; resource exhaustion is an explicit adapter outcome, not an optimizer rewrite or an alteration of SQL transaction semantics.

## Verification obligations

The original notification integration harness exercises the optional adapter against in-memory and native SQLite providers, transaction and savepoint deferral, sender identity, channel rollback, bounded native scanning, immutable configuration, count/byte exhaustion and repeated terminal reads. Existing legacy and independent-subscription tests continue to exercise the unchanged branches. The actual Linux and macOS performance regression CI must complete for the final source before it is merged or called fully verified.
