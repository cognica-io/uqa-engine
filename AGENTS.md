# Agent instructions

Read `llms.txt` first, then [AI_POLICY.md](AI_POLICY.md).

For UQA Engine SQL, documentation, and feature verification, follow `.agents/skills/uqa-engine/SKILL.md`.

The manual is authoritative for public behavior. Verify ambiguous claims against implementation and tests.

Before implementing or moving functionality, inspect the relevant crates' `Cargo.toml` files, including declared and enabled Cargo features, `scripts/workspace-dependency-policy.json`, the manual's ownership boundaries, and existing implementations and tests. Confirm which crate owns the behavior, what it already supports under the relevant feature configuration, and the permitted dependency direction before choosing an implementation location.

Implement behavior in its owning crate and keep its tests there. Keep Engine responsible for state, session, transaction, and retained-resource adapters; do not place analysis, planning, scoring, storage, or execution algorithms in Engine for convenience. Reuse or extend the owning crate's interfaces instead of duplicating behavior, adding reverse dependencies, or weakening dependency and capability policies. Run the relevant ownership and dependency checks before committing.

Do not modify external dependency sources, forks or package patches, or introduce private dependency APIs, without explicit maintainer authorization. Use the dependencies' existing interfaces. Instrument only activity that the project owns or that existing APIs can attribute accurately; document unavailable measurements instead of inventing counters or expanding the work into dependency modifications. For the C binding delivery, page I/O coverage is explicitly limited to this available instrumentation.

Do not patch state or lifecycle defects with caller-side `ensure_*` checks, repeated validation, ad hoc state resets, or special-case execution paths. The owning state machine must enforce operation admission, valid transitions, and failure or rollback handling within its execution boundary; callers must not be responsible for making internal state usable before invoking it. Fix the transition or ownership model at its source and route all entry points through that boundary. Renaming a guard or moving it into a helper without correcting the lifecycle is not a fix.

Keep generated benchmark reports and diagnostic traces in ignored output directories or CI artifacts. Commit only workload fixtures, expected results, limits, and compact provenance references; verifier unit tests must not depend on historical machine reports. Performance acceptance requires a controlled host and an independently established noise bound. Do not retry uncontrolled measurements until they pass, infer host control from matching metadata or a clean worktree, or delay independent code review and fixes while waiting for noisy timing results.

Always check the automatic performance regression CI for the product changes being delivered before merging or reporting verification complete. Verify the tested commit and the actual Linux and macOS regression results, including bounded work, memory and output correctness; successful build or archive jobs alone are insufficient. Pending, skipped, cancelled or failed checks are not passing evidence; investigate failures and fix their causes. Let the configured repository events trigger CI; do not manually dispatch, rerun or cancel workflows.

Group related open issues by their owning behavior and dependencies, use one PR per coherent group, and commit individual fixes in logical units. Create implementation PRs as drafts and push those commits without starting CI for each update. Finish the scoped implementation, focused local verification, review fixes and current task records before marking a PR ready for review; that transition runs the affected PR checks once for the completed batch, including automatic Linux/macOS performance regression checks. If qualification requires a product fix, return the PR to draft, finish the correction batch and mark it ready again. Before merging, verify the actual final-source results; reuse completed evidence only for identical qualification inputs. A cancelled or skipped run is not evidence. Main pushes retain their configured workspace and performance checks; the FDW matrix runs only for affected ready-for-review candidates, with all six Linux/macOS configurations verified against the final source fingerprint.

Qualify a dependent PR stack as one integrated candidate. Complete and combine its implementation branches before starting the full matrix; do not mark intermediate stacked PRs ready or wait for duplicate qualification of their cumulative sources. Pull-request workflows target `main`, so only the final candidate's ready transition starts qualification. A new completed ready candidate automatically supersedes obsolete runs for the same PR; draft and documentation pushes do not cancel qualification, and main or release runs keep their configured execution. Preserve logical commits, ownership boundaries and focused local evidence for each unit, reconcile superseded PRs and evaluate the complete diff against `main` when selecting affected checks.

Keep each PR at 10,000 changed lines or fewer, counting additions plus deletions against its actual base. If a PR exceeds that limit, stop expanding it and split the work and PRs by owning behavior and dependency order before review or merge. Each resulting PR must contain a coherent, independently verifiable change within the limit; use the prerequisite branch as the base for dependent work. Record transferred scope, dependency links and verification requirements in the affected issues and PRs. Filing follow-up issues alone does not resolve an oversized existing diff.

PR #1343 is the integration-only exception for the existing backup stack #1324, #1331–#1335 and #1340–#1342. Its existing product commits are consolidated for one final-source qualification; do not expand its product scope. Remaining backup implementation stays in #1322 and subsequent bounded PRs.

PR #1344 is the integration-only exception for the existing catalog and memory-membership stack #1344 and #1347 through product source `5511ad6cf3f4e838ab9a4ec3a175bd3aa51e42e4`. Its 10,503 changed product/documentation lines against main `491ad0dbd93fd7d0ccecb15caa61ca6c05beb185` are consolidated for one final-source qualification. Only integration policy and delivery records may be added to that candidate; subsequent implementation remains in bounded PRs under #1346 and #1322.

PR #1361 is the integration-only exception for the existing backup source and image stack #1358, #1360 and #1361 through product source `7d0626bff80c603da54b5cb62ae3191c4645b329`. Its 26,822 changed product/documentation lines against main `3c565c555d44ba34774d7f3d30690258ec7018d0` are consolidated for one final-source qualification. Only integration policy, delivery records, the memory-posting-cursor allocation correction and atomic publication of the retention fixture readiness signal found during integrated qualification may be added to that candidate; remaining backup implementation stays in #1322 and subsequent bounded PRs. Delivery qualification for that test-only correction combines the completed production-source CI matrix, Linux/macOS retention tests and completed macOS doctests.

Keep each prose paragraph on one physical line. Do not insert line breaks inside paragraphs.

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
