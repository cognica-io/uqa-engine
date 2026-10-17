#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Import or verify the pinned PostgreSQL 18.4 regular expression engine as uqa-pg-regex."""

from __future__ import annotations

import argparse
import hashlib
import pathlib
import subprocess
import sys
import tempfile


ROOT = pathlib.Path(__file__).resolve().parents[1]
CRATE = ROOT / "crates" / "uqa-pg-regex"
IMPORT_ROOT = "postgres"
CHECKSUMS = CRATE / "SHA256SUMS"
UPSTREAM = CRATE / "UPSTREAM.md"
POSTGRESQL_REPOSITORY = "https://github.com/postgres/postgres.git"
POSTGRESQL_TAG = "REL_18_4"
POSTGRESQL_COMMIT = "f5cc81719e6da4cbdb1f797c48b693e91018153a"

# Paths relative to the PostgreSQL source root. Each file is written unchanged to postgres/<path>.
IMPORTED_FILES = (
    "COPYRIGHT",
    "src/backend/regex/COPYRIGHT",
    "src/backend/regex/regc_color.c",
    "src/backend/regex/regc_cvec.c",
    "src/backend/regex/regc_lex.c",
    "src/backend/regex/regc_locale.c",
    "src/backend/regex/regc_nfa.c",
    "src/backend/regex/regcomp.c",
    "src/backend/regex/rege_dfa.c",
    "src/backend/regex/regerror.c",
    "src/backend/regex/regexec.c",
    "src/backend/regex/regfree.c",
    "src/common/unicode_case.c",
    "src/common/unicode_category.c",
    "src/include/common/unicode_case.h",
    "src/include/common/unicode_case_table.h",
    "src/include/common/unicode_category.h",
    "src/include/common/unicode_category_table.h",
    "src/include/mb/pg_wchar.h",
    "src/include/regex/regcustom.h",
    "src/include/regex/regerrs.h",
    "src/include/regex/regex.h",
    "src/include/regex/regguts.h",
)

ADAPTATIONS = (
    "No imported file is edited. The build compiles the imported translation units `regcomp.c`, `regexec.c`, `regerror.c`, `regfree.c`, `unicode_category.c` and `unicode_case.c` together with the UQA-owned `shim/uqa_pg_regex.c`, with `shim/include` and `shim/regex` ahead of `postgres/src/include` on the include path, at `-O2` in every build profile as PostgreSQL compiles them, because the stack the engine's recursion uses depends on the optimization level.",
    "`shim/include/postgres.h`, `miscadmin.h` and `utils/palloc.h` supply the c.h-level types and macros, the memory allocation interface and the interrupt and stack checks that the imported files use; `mb/pg_wchar.h` is PostgreSQL's own header. Assertions are compiled out as in production PostgreSQL builds, where `USE_ASSERT_CHECKING` is not defined. `pg_char_and_wchar_strncmp`, which PostgreSQL defines in `src/backend/utils/mb/wstrncmp.c`, is implemented in the shim with the same comparison.",
    "The imported `regcustom.h` routes `MALLOC`, `REALLOC`, `FREE`, `MALLOC_ARRAY` and `REALLOC_ARRAY` to `palloc_extended` and its relatives with `MCXT_ALLOC_NO_OOM`. The shim implements them over per-operation memory contexts owned by the Rust wrapper, which charges every chunk, including its header, to the caller's resource hooks before allocating and releases it after freeing. A refused reservation or failed allocation returns NULL, so the engine reports `REG_ESPACE`. Requests above `MaxAllocSize` and overflowing array sizes raise the same errors as PostgreSQL's memory-context code.",
    "`CHECK_FOR_INTERRUPTS()`, which the imported `INTERRUPT()` macro uses, and `stack_is_too_deep()`, which backs `STACK_TOO_DEEP()`, call the caller's cancellation and stack-depth hooks. Stack depth is measured from the entry of the current engine operation. Beyond PostgreSQL's default `max_stack_depth` of 2 MB, or a lower limit of the hooks, the engine reports `REG_ETOOBIG` as in PostgreSQL. Every operation runs with stack for that depth and PostgreSQL's `STACK_DEPTH_SLOP` of 512 KiB beyond it: on the thread's own stack when it has that much, and otherwise on a separately allocated stack of the same thread through the `stacker` crate. An operation whose thread lacks that headroom first runs on the thread's stack while its depth leaves the slop free; a deeper `stack_is_too_deep()` call abandons it through the operation's error frame, and the wrapper runs it again on a separate stack. WebAssembly runs the engine's calls on its own call stack, which can be neither measured nor switched, so there the depth covers only the stack in linear memory and operations run where they are.",
    "PostgreSQL leaves the regular expression code through `ereport(ERROR)` and a `longjmp` to its error handler. The shim keeps that structure entirely in C: every engine call runs inside a UQA-owned C function that establishes a `sigsetjmp` frame without saving the signal mask, selected as PostgreSQL's `c.h` selects it (plain `setjmp` on Windows and the compiler builtins on MinGW-w64), and cancellation, panics of the Rust hooks, a stack too small for the operation and allocation-size errors jump only to that frame, never across Rust frames or between stacks. The Rust wrapper then frees every chunk of the abandoned operation, as PostgreSQL resets the memory context of a failed query.",
    "A compiled expression is freed by deleting its memory context, as `RE_compile_and_cache` deletes the context of an expression it discards, so `pg_regfree` is compiled but not called.",
    "`src/backend/regex/regc_pg_locale.c` is not imported. `regcomp.c` includes `shim/regex/regc_pg_locale.c` instead, a port that keeps the selected locale strategy in the per-thread engine frame rather than in process-global variables, so concurrent operations on different threads are independent. It implements PostgreSQL's C, builtin and ICU strategies: the C strategy uses PostgreSQL's ASCII property table, the builtin strategy calls the imported `unicode_category.c` and `unicode_case.c` with the provider's `casemap_full` setting, and the ICU strategy evaluates ICU4C's `u_is*`, `u_toupper` and `u_tolower` definitions over ICU4X Unicode 16 data. The libc strategies are not provided because UQA defines no libc collation other than C and POSIX. Character-class cvecs, which PostgreSQL caches per backend in malloc'd memory, are kept complete once per process in immutable Rust storage, so a class reference allocates nothing, as a hit in PostgreSQL's cache does.",
    "The engine's `Oid collation` argument carries a UQA locale token chosen by the Rust wrapper after it has applied PostgreSQL's collation checks, so the `ereport` paths of PostgreSQL's `pg_set_regex_collation` are rejected before C runs.",
    "Global symbols defined by the imported files and the shim are renamed with a `uqa_pg_regex_` prefix through macros in `shim/include/postgres.h`, so they cannot collide with the PostgreSQL parser objects that `uqa-pg-query` links into the same binary.",
)


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_file(path: pathlib.Path) -> str:
    return sha256_bytes(path.read_bytes())


def destination(relative: str) -> pathlib.Path:
    return CRATE / IMPORT_ROOT / relative


def checksum_entries() -> list[tuple[str, str]]:
    return [(f"{IMPORT_ROOT}/{relative}", sha256_file(destination(relative))) for relative in IMPORTED_FILES]


def render_checksums(entries: list[tuple[str, str]]) -> str:
    return "".join(f"{digest}  {relative}\n" for relative, digest in entries)


def render_upstream(entries: list[tuple[str, str]]) -> str:
    lines = [
        "# uqa-pg-regex upstream pin",
        "",
        "This crate compiles PostgreSQL's Advanced Regular Expression engine, Henry Spencer's library as maintained in `src/backend/regex`, from unchanged PostgreSQL sources.",
        "",
        f"- Repository: `{POSTGRESQL_REPOSITORY}`",
        f"- Tag: `{POSTGRESQL_TAG}` (PostgreSQL 18.4)",
        f"- Commit: `{POSTGRESQL_COMMIT}`",
        "",
        f"`python3 scripts/sync-uqa-pg-regex.py` writes every file below unchanged to `{IMPORT_ROOT}/` at its PostgreSQL path and regenerates `SHA256SUMS` and this file. `python3 scripts/sync-uqa-pg-regex.py --check` verifies the tree offline. An update must change the pinned tag and commit in the script, review the upstream diff of every listed file, and regenerate both files in the same change.",
        "",
        "## Imported files",
        "",
        "| File | SHA-256 |",
        "| --- | --- |",
    ]
    lines.extend(f"| `{relative}` | `{digest}` |" for relative, digest in entries)
    lines.extend(["", "## Local adaptations", ""])
    lines.extend(f"- {adaptation}" for adaptation in ADAPTATIONS)
    lines.extend([
        "",
        "## Licenses",
        "",
        f"The imported files keep their notices: `{IMPORT_ROOT}/COPYRIGHT` is the PostgreSQL License, and `{IMPORT_ROOT}/src/backend/regex/COPYRIGHT` carries Henry Spencer's license for the regular expression package and the Tcl license for portions of `regc_locale.c`. `LICENSE` states the terms of the UQA-owned wrapper, build script and shims.",
        "",
    ])
    return "\n".join(lines)


def run(command: list[str], cwd: pathlib.Path | None = None) -> bytes:
    result = subprocess.run(command, cwd=cwd, check=True, capture_output=True)
    return result.stdout


def resolve_commit(source: pathlib.Path, revision: str) -> str:
    return run(["git", "rev-parse", "--verify", f"{revision}^{{commit}}"], cwd=source).decode().strip()


def verify_source(source: pathlib.Path) -> None:
    commit = resolve_commit(source, POSTGRESQL_COMMIT)
    if commit != POSTGRESQL_COMMIT:
        raise RuntimeError(f"{source} resolves the pinned commit to {commit}")
    try:
        tagged = resolve_commit(source, f"refs/tags/{POSTGRESQL_TAG}")
    except subprocess.CalledProcessError as error:
        raise RuntimeError(f"{source} has no {POSTGRESQL_TAG} tag") from error
    if tagged != POSTGRESQL_COMMIT:
        raise RuntimeError(f"{POSTGRESQL_TAG} is {tagged}, expected {POSTGRESQL_COMMIT}")


def clone_source(work_dir: pathlib.Path) -> pathlib.Path:
    source = work_dir / "postgres"
    run([
        "git", "clone", "--quiet", "--depth", "1", "--single-branch",
        "--branch", POSTGRESQL_TAG, POSTGRESQL_REPOSITORY, str(source),
    ])
    return source


def import_files(source: pathlib.Path) -> None:
    verify_source(source)
    contents = {relative: run(["git", "cat-file", "blob", f"{POSTGRESQL_COMMIT}:{relative}"], cwd=source)
                for relative in IMPORTED_FILES}
    import_root = CRATE / IMPORT_ROOT
    if import_root.exists():
        for path in sorted(import_root.rglob("*"), reverse=True):
            if path.is_dir():
                path.rmdir()
            else:
                path.unlink()
    for relative, data in contents.items():
        path = destination(relative)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
    entries = checksum_entries()
    CHECKSUMS.write_text(render_checksums(entries), encoding="utf-8")
    UPSTREAM.write_text(render_upstream(entries), encoding="utf-8")


def parse_checksums(text: str) -> list[tuple[str, str]]:
    entries = []
    for number, line in enumerate(text.splitlines(), start=1):
        digest, separator, relative = line.partition("  ")
        if not separator or len(digest) != 64 or any(character not in "0123456789abcdef" for character in digest):
            raise RuntimeError(f"{CHECKSUMS}:{number} is not a SHA-256 checksum line")
        entries.append((relative, digest))
    return entries


def check_tree() -> int:
    try:
        recorded = parse_checksums(CHECKSUMS.read_text(encoding="utf-8"))
    except OSError as error:
        raise RuntimeError(f"cannot read {CHECKSUMS}: {error}") from error
    expected_paths = [f"{IMPORT_ROOT}/{relative}" for relative in IMPORTED_FILES]
    if [relative for relative, _ in recorded] != expected_paths:
        raise RuntimeError("SHA256SUMS does not list exactly the pinned PostgreSQL files in order")
    import_root = CRATE / IMPORT_ROOT
    actual = sorted(path.relative_to(CRATE).as_posix() for path in import_root.rglob("*") if path.is_file())
    missing = sorted(set(expected_paths) - set(actual))
    extra = sorted(set(actual) - set(expected_paths))
    changed = sorted(relative for relative, digest in recorded
                     if relative not in missing and sha256_file(CRATE / relative) != digest)
    if missing or extra or changed:
        details = []
        for label, paths in (("missing", missing), ("extra", extra), ("changed", changed)):
            if paths:
                details.append(f"{label}: " + ", ".join(paths))
        raise RuntimeError("uqa-pg-regex import drifted from SHA256SUMS (" + "; ".join(details) + ")")
    try:
        upstream = UPSTREAM.read_text(encoding="utf-8")
    except OSError as error:
        raise RuntimeError(f"cannot read {UPSTREAM}: {error}") from error
    if upstream != render_upstream(recorded):
        raise RuntimeError("UPSTREAM.md differs from the pinned import; run scripts/sync-uqa-pg-regex.py")
    return len(recorded)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="verify the imported tree against SHA256SUMS without network access")
    parser.add_argument("--source", type=pathlib.Path, help="existing PostgreSQL checkout that contains the pinned tag and commit")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if args.check:
        count = check_tree()
        print(f"uqa-pg-regex import OK: {count} files at {POSTGRESQL_TAG} ({POSTGRESQL_COMMIT})")
        return 0
    if args.source is not None:
        import_files(args.source.resolve())
    else:
        with tempfile.TemporaryDirectory(prefix="uqa-pg-regex-sync-") as directory:
            import_files(clone_source(pathlib.Path(directory)))
    print(f"Imported {len(IMPORTED_FILES)} files from {POSTGRESQL_TAG} into {CRATE / IMPORT_ROOT}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, RuntimeError, subprocess.CalledProcessError) as error:
        detail = getattr(error, "stderr", None)
        if isinstance(detail, bytes) and detail:
            print(detail.decode(errors="replace"), file=sys.stderr)
        print(error, file=sys.stderr)
        raise SystemExit(1) from error
