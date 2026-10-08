# Concurrent key reservation work

The shared claim table in #266 replaces one OS byte-range lock per row or key with exact entries and a process liveness lease. Subsequent fixes bound mapped I/O, group already evaluated key reservations, preserve complete identities, exclude incompatible owners and reclaim local identities after their final owner. The remaining structural check must exercise several actual processes through `RowLockManager`, not only a coordinator or several threads in one manager.

The owning Execution test starts two and four independent processes, each retaining 64 or 512 distinct full-digest keys. All peers report readiness before acquisition commands are sent, and the parent does not await any acquisition response until all commands have been sent. Each peer checks all returned grants, exactly $\lceil N/64 \rceil$ claim-table arbitrations, no relation registry entries/native pins for its keys, and complete local identity reclamation after release. The parent probes the first and last key of every peer before and after each independent release, requiring `55P03` only while that exact peer still owns the key. Each database is a fresh disposable fixture, and a failed parent drops and joins its subprocesses.

This validates bounded arbitration counts and independent ownership under concurrent process schedules. It does not measure mutex wait time or decide that partitioned locking is unnecessary. Controlled contention latency and throughput remain part of the unresolved timing qualification tracked in #261 and #266; shared-host timings are not acceptance evidence.

- [x] Inspect Execution's existing batch, full-identity and subprocess fixtures and reuse its established owner APIs.
- [x] Add the independent-process schedule and automatic regression inventory entry without product/dependency changes.
- [x] Pass all six focused batch cases, strict Execution Clippy, formatting and ownership/dependency, harness, file-size and header checks.
- [x] Complete the implementation, review and automatic Linux/macOS verification required for PR #592.

The focused batch suite passed six cases, including the four multi-process schedules, with the subprocess entry point intentionally ignored outside its parent fixture. Strict Execution Clippy and repository ownership/dependency, harness, file-size/header and inventory checks passed. The exact-process schedule and failure cleanup were reviewed directly; CodeRabbit and Copilot were quota-limited.

Product commit `dc9562da6` passed all 97 required cases on both Linux and macOS in [run 37711567801](https://github.com/cognica-io/uqa-engine/actions/runs/37711567801). The PR formatting check also passed; the final commit only records this verification.
