# Agent instructions

Read `llms.txt` first.

For UQA Engine SQL, documentation, and feature verification, follow `.agents/skills/uqa-engine/SKILL.md`.

The manual is authoritative for public behavior. Verify ambiguous claims against implementation and tests.

Before implementing or moving functionality, inspect the relevant crates' `Cargo.toml` files, including declared and enabled Cargo features, `scripts/workspace-dependency-policy.json`, the manual's ownership boundaries, and existing implementations and tests. Confirm which crate owns the behavior, what it already supports under the relevant feature configuration, and the permitted dependency direction before choosing an implementation location.

Implement behavior in its owning crate and keep its tests there. Keep Engine responsible for state, session, transaction, and retained-resource adapters; do not place analysis, planning, scoring, storage, or execution algorithms in Engine for convenience. Reuse or extend the owning crate's interfaces instead of duplicating behavior, adding reverse dependencies, or weakening dependency and capability policies. Run the relevant ownership and dependency checks before committing.

Keep each prose paragraph on one physical line. Do not insert line breaks inside paragraphs.

Preserve the established `CPU`, `MLX`, and `UQA` initialisms in Rust identifiers and design pseudocode; do not apply mixed-case acronym normalization to them.

Do not put rollout phase identifiers in source comments, documentation comments, test names, or permanent policy labels; describe enduring ownership or behavior instead.

Name feature branches with the `feature/` prefix and bug-fix branches with the `fix/` prefix. Do not use any other branch prefix.

Do not prefix commit messages or pull request titles with labels such as `feat:`, `fix:`, `chore:`, `doc:`, or `docs:`.

Keep exactly one test executable per crate. Put additional integration-test files in submodules of that crate's single test target; never add another top-level `tests/*.rs` file or `[[test]]` target.

Treat every behavior difference from PostgreSQL 18 as a bug. Fix the implementation; do not waive or merely document the difference as a compatibility gap.
