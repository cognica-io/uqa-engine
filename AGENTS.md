# Agent instructions

Read `llms.txt` first, then [AI_POLICY.md](AI_POLICY.md).

For UQA Engine SQL, documentation, and feature verification, follow `.agents/skills/uqa-engine/SKILL.md`.

The manual is authoritative for public behavior. Verify ambiguous claims against implementation and tests.

Before adding an implementation, search the workspace for existing implementations, interfaces and tests of the same behavior and evaluate whether they can be reused or extended. Prefer the existing owning implementation and thin adapters over a second algorithm, constructor, traversal or state machine for another caller, API, backend or execution mode. Synchronous and resumable paths must share their semantic implementation where they perform the same operation. When a separate implementation is necessary, document the concrete technical constraint in the PR; remove superseded duplicate code when its callers migrate. Review the completed change for duplicate behavior and verify the affected callers use the intended shared implementation.

Before implementing or moving functionality, inspect the relevant crates' `Cargo.toml` files, including declared and enabled Cargo features, `scripts/workspace-dependency-policy.json`, the manual's ownership boundaries, and existing implementations and tests. Confirm which crate owns the behavior, what it already supports under the relevant feature configuration, and the permitted dependency direction before choosing an implementation location.

Implement behavior in its owning crate and keep its tests there. Keep Engine responsible for state, session, transaction, and retained-resource adapters; do not place analysis, planning, scoring, storage, or execution algorithms in Engine for convenience. Reuse or extend the owning crate's interfaces instead of duplicating behavior, adding reverse dependencies, or weakening dependency and capability policies. Run the relevant ownership and dependency checks before committing.

Do not modify external dependency sources, forks or package patches, or introduce private dependency APIs, without explicit maintainer authorization. Use the dependencies' existing interfaces. Instrument only activity that the project owns or that existing APIs can attribute accurately; document unavailable measurements instead of inventing counters or expanding the work into dependency modifications. For the C binding delivery, page I/O coverage is explicitly limited to this available instrumentation.

Do not patch state or lifecycle defects with caller-side `ensure_*` checks, repeated validation, ad hoc state resets, or special-case execution paths. The owning state machine must enforce operation admission, valid transitions, and failure or rollback handling within its execution boundary; callers must not be responsible for making internal state usable before invoking it. Fix the transition or ownership model at its source and route all entry points through that boundary. Renaming a guard or moving it into a helper without correcting the lifecycle is not a fix.

Keep generated benchmark reports and diagnostic traces in ignored output directories or CI artifacts. Commit only workload fixtures, expected results, limits, and compact provenance references; verifier unit tests must not depend on historical machine reports. Performance acceptance requires a controlled host and an independently established noise bound. Do not retry uncontrolled measurements until they pass, infer host control from matching metadata or a clean worktree, or delay independent code review and fixes while waiting for noisy timing results.

Investigate every observed test failure, including a single failure in a local or auxiliary run. Record the failing assertion, its cause and the verification of the fix. A passing rerun or a pass under different isolation or concurrency settings leaves the original failure open until its cause is established.

Run focused local tests for the final PR source and verify the lightweight PR checks before merging. The push to main runs the full workspace and affected product, binding and performance checks. Before reporting verification complete, check the merged revision and the actual Linux and macOS regression results for bounded work, memory and output correctness. Investigate every failed or incomplete check. A successful build or archive alone does not verify test execution. Let configured repository events trigger CI. Do not manually dispatch, rerun or cancel workflows.

Group related open issues by their owning behavior and dependencies, use one PR per coherent group, and commit individual fixes in logical units. Push implementation and review fixes to a draft. Once local verification and task records are complete, mark the PR ready to run formatting, repository policy, HTTP client tests and any affected manifest audit. Return to draft for corrections and check the completed batch at its next ready transition.

Combine dependent implementation branches into one candidate before review. Preserve logical commits, ownership boundaries and focused local evidence for each unit, and reconcile superseded PRs. Lightweight PR checks target the final candidate against main. Run the full matrix once on main after merge. For affected FDW changes, verify all six Linux/macOS configurations for the merged input fingerprint. Reuse completed evidence only for identical qualification inputs.

Keep each PR at 10,000 changed lines or fewer, counting additions plus deletions against its actual base. If a PR exceeds that limit, stop expanding it and split the work and PRs by owning behavior and dependency order before review or merge. Each resulting PR must contain a coherent, independently verifiable change within the limit; use the prerequisite branch as the base for dependent work. Record transferred scope, dependency links and verification requirements in the affected issues and PRs. Filing follow-up issues alone does not resolve an oversized existing diff.

Keep each prose paragraph on one physical line. Do not insert line breaks inside paragraphs in the Markdown file.

Describe decisions, scope, behavior and verification evidence directly in issues, PRs and documentation. Do not narrate instructions, approvals or conversation history, or attribute a decision or requirement to a person.

Preserve the established `CPU`, `MLX`, and `UQA` initialisms in Rust identifiers and design pseudocode; do not apply mixed-case acronym normalization to them.

Do not put rollout phase identifiers in source comments, documentation comments, test names, or permanent policy labels; describe enduring ownership or behavior instead.

Name feature branches with the `feature/` prefix and bug-fix branches with the `fix/` prefix. Do not use any other branch prefix.

Associate each implementation branch with a pull request when first pushing it. Record its task, scope, dependency PRs and current verification state in that PR. Keep dependent diffs limited to their own logical unit by selecting the appropriate base branch. Finish verified deliveries, close their resolved issues and remove merged branches promptly; do not leave pushed work on branches without PRs or keep expanding a queue of unfinished branches and PRs.

Apply the repository's existing type and applicable area labels when creating issues and pull requests, following comparable recent items and adding relevant concern labels where appropriate. Read back the created item to verify its labels, linked tasks and current scope; do not leave classification for a later pass or invent a new label when an established one fits.

Preserve inherited task descriptions and scope while keeping their checkboxes, completion evidence and remaining work current. Reconcile the original task list, current plan and PR body whenever work is completed or transferred to a successor PR. Mark a task complete only with the required verification and delivery evidence; describe partial completion and the exact remaining requirements on unchecked items. Keep any historical snapshot explicitly separate from live progress. Never freeze a live checklist merely to preserve its original wording.

Do not prefix commit messages or pull request titles with labels such as `feat:`, `fix:`, `chore:`, `doc:`, or `docs:`.

Keep exactly one test executable per crate. Put additional integration-test files in submodules of that crate's single test target; never add another top-level `tests/*.rs` file or `[[test]]` target.

Treat every behavior difference from PostgreSQL 18 as a bug. Fix the implementation; do not waive or merely document the difference as a compatibility gap. The exception in [#656](https://github.com/cognica-io/uqa-engine/issues/656) rejects retained composite reads beyond their owned tuple with `XX001` instead of reproducing PostgreSQL allocator contents; preserve all defined in-bounds, lazy-read, transaction and persistence behavior.
