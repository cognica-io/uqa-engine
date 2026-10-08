# Completion of retained transaction receipts

Issue #347 identifies the cost of durable autonomous sequence logs. SQLite currently publishes each managed transaction and then opens another physical writer solely to mark its terminal receipt acknowledged. The provider already has authoritative managed-owner leases and reclaims a terminal receipt after the final lease disappears, while independently retaining every durable SSI reference. Common Storage owns completion ordering; SQLite owns physical acknowledgement and reclamation. Their existing dependency direction is sufficient. No Engine algorithm, Cargo dependency, feature, sequence allocation rule or durable format changes.

`VersionedPersistence::acknowledge_retained_transaction` receives the retained owner and the exact confirmed outcome. Its default calls the established explicit acknowledgement method, preserving custom and redb provider behavior. SQLite checks the database incarnation, allocation watermark, exact terminal outcome and durable managed flag in one read snapshot. Only an owner with a resolution lease and a managed durable allocation can complete without a new physical writer. Untracked or manual ownership continues through explicit durable acknowledgement. Cancellation and mismatches return an error while Common Storage keeps the completed transaction and owner for retry.

Let $R(t)$ be the authoritative durable receipt of transaction $t$, $L(t)$ its live managed lease and $S(t)$ any retained SSI reference. Publication establishes $R(t)$ before completion. Successful retained completion validates $R(t)$ without changing it; the session then drops its transaction frame and $L(t)$. Reclamation still requires $\neg L(t) \land \neg S(t)$ and decodes $R(t)$ before removal. A process failure before or after completion therefore has the same durable outcome and existing recovery eligibility as abandoned managed ownership. If the session survives an error, $L(t)$ protects its exact retry evidence. Sequence values, generation checks, cache/lookahead rules and publication synchronization remain unchanged.

- [x] Inspect Common Storage/SQLite dependencies, features, session completion, receipt leases and bounded recovery.
- [x] Add the conservative provider capability and SQLite read-only terminal validation.
- [ ] Verify no acknowledgement commit, live-owner protection, manual fallback, errors, cancellation, SSI overlap and autonomous sequence writes.
- [ ] Pass focused provider/common/sequence checks, formatting, strict Clippy and ownership/dependency checks.
- [ ] Update the manual, HISTORY and regression inventory; commit, push, review, merge and clean up.

Acceptance uses physical commit counts and exact state/recovery assertions. It makes no latency claim on the shared local host. Each required sequence log still needs its durable publication; this change removes its redundant acknowledgement transaction.
