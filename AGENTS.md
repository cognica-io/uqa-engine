# Agent instructions

Read `llms.txt` first.

For UQA-RS SQL, documentation, and feature verification, follow `.agents/skills/uqa-rs/SKILL.md`.

The manual is authoritative for public behavior. Verify ambiguous claims against implementation and tests.

Keep each prose paragraph on one physical line. Do not insert line breaks inside paragraphs.

Name feature branches with the `feature/` prefix and bug-fix branches with the `fix/` prefix. Do not use any other branch prefix.

Keep exactly one test executable per crate. Put additional integration-test files in submodules of that crate's single test target; never add another top-level `tests/*.rs` file or `[[test]]` target.

Treat every behavior difference from PostgreSQL 18 as a bug. Fix the implementation; do not waive or merely document the difference as a compatibility gap.
