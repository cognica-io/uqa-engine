# uqa-pg-regex upstream pin

This crate compiles PostgreSQL's Advanced Regular Expression engine, Henry Spencer's library as maintained in `src/backend/regex`, from unchanged PostgreSQL sources.

- Repository: `https://github.com/postgres/postgres.git`
- Tag: `REL_18_4` (PostgreSQL 18.4)
- Commit: `f5cc81719e6da4cbdb1f797c48b693e91018153a`

`python3 scripts/sync-uqa-pg-regex.py` writes every file below unchanged to `postgres/` at its PostgreSQL path and regenerates `SHA256SUMS` and this file. `python3 scripts/sync-uqa-pg-regex.py --check` verifies the tree offline. An update must change the pinned tag and commit in the script, review the upstream diff of every listed file, and regenerate both files in the same change.

## Imported files

| File | SHA-256 |
| --- | --- |
| `postgres/COPYRIGHT` | `3d6af92ff8a4c2cdf69afb1cf44edea727922f5cd0cf8b5f72b11cdecac8fdfd` |
| `postgres/src/backend/regex/COPYRIGHT` | `5027cfe4f54842896b01ca6be28becc9a018623aaadc0f5b51350da71118cb0e` |
| `postgres/src/backend/regex/regc_color.c` | `04141f1c53d22e5d2a623562ab24124e7bf828b2a0358c27175f0a207d5aefcc` |
| `postgres/src/backend/regex/regc_cvec.c` | `a2a4ae31cf5761338aa600ec3db34186684e61225c13208e6629ff747ef35258` |
| `postgres/src/backend/regex/regc_lex.c` | `246ef4cae1691571f8d8dbd475cf4186c24ec4137e716b7a096568082c21fdc7` |
| `postgres/src/backend/regex/regc_locale.c` | `41baf773ac857bc8cebffdb2f0ea3130d645d91ebbbc6d28b861fa160d11c276` |
| `postgres/src/backend/regex/regc_nfa.c` | `8b22947753ce169a2968a31172b5b20bf02eba00131a195d943f1d4a232d1a1e` |
| `postgres/src/backend/regex/regcomp.c` | `b6f776012ef48a03397d0c63eebc6e27a042080ad353baec3bc579f6c4828819` |
| `postgres/src/backend/regex/rege_dfa.c` | `b0c8026590e98d70bd7609cb075cdd24ad6943026ffc1c8cb9a50be78a640140` |
| `postgres/src/backend/regex/regerror.c` | `db9383bc6b0fcf8fd7f92df67e826443abca8bf0e3db16118918cfe86520d2cb` |
| `postgres/src/backend/regex/regexec.c` | `21ade6f3bc350207299b020eef4d083575e5f8369c6a80ebe0a475d5c61371d7` |
| `postgres/src/backend/regex/regfree.c` | `2790f8e84c845a0f40cd495d00cb3e0989b48a80cfb8d948ccd5400ce3217a6b` |
| `postgres/src/common/unicode_case.c` | `e56db3eeaf4086bf1bbe749667a89f63cf7e2017a6980be23a675c261f55a842` |
| `postgres/src/common/unicode_category.c` | `0fb9ff85b63f21eb89c9f7fb089eb6731fe2e007867623361abfb1fc22f93e16` |
| `postgres/src/include/common/unicode_case.h` | `2687766e0dccbfdd3da2a11003f9c6ae1c2b02a66e79b5b76b3765587dc52db7` |
| `postgres/src/include/common/unicode_case_table.h` | `b6080b036284203448a9a4a2abc408f3a8983dedd5affb37288427082bc67a2c` |
| `postgres/src/include/common/unicode_category.h` | `c37f437fc087b7d3f6898c82f35661a29bd161629ec1893aae37c1b790221ba8` |
| `postgres/src/include/common/unicode_category_table.h` | `fcdd8d788621326ecf91f3167de2e053da8280817509ad7497aa2ef17c430866` |
| `postgres/src/include/mb/pg_wchar.h` | `83d2d3e37627f685dac94fc8fc4549b35099f4acdbd273fe0a4d3cdad735e3c7` |
| `postgres/src/include/regex/regcustom.h` | `fa97bc14bcb05115a8e02ea2568ad68546a50da6027c436f315061e3b59d641c` |
| `postgres/src/include/regex/regerrs.h` | `08576f230016af3458d0b21e516f3daab49ac01cee92b61a93909a8232e180bc` |
| `postgres/src/include/regex/regex.h` | `f798e6a7c2099d29b9b7d635d3ea61811060d3cb7386b6c23bde33b5c3230ed8` |
| `postgres/src/include/regex/regguts.h` | `7d0cd2d3ad8f82ce93fc06c78c8e40e494d1ffea99e27808abb6e1c51be501a3` |

## Local adaptations

- No imported file is edited. The build compiles the imported translation units `regcomp.c`, `regexec.c`, `regerror.c`, `regfree.c`, `unicode_category.c` and `unicode_case.c` together with the UQA-owned `shim/uqa_pg_regex.c`, with `shim/include` and `shim/regex` ahead of `postgres/src/include` on the include path, at `-O2` in every build profile as PostgreSQL compiles them, because the stack the engine's recursion uses depends on the optimization level.
- `shim/include/postgres.h`, `miscadmin.h` and `utils/palloc.h` supply the c.h-level types and macros, the memory allocation interface and the interrupt and stack checks that the imported files use; `mb/pg_wchar.h` is PostgreSQL's own header. Assertions are compiled out as in production PostgreSQL builds, where `USE_ASSERT_CHECKING` is not defined. `pg_char_and_wchar_strncmp`, which PostgreSQL defines in `src/backend/utils/mb/wstrncmp.c`, is implemented in the shim with the same comparison.
- The imported `regcustom.h` routes `MALLOC`, `REALLOC`, `FREE`, `MALLOC_ARRAY` and `REALLOC_ARRAY` to `palloc_extended` and its relatives with `MCXT_ALLOC_NO_OOM`. The shim implements them over per-operation memory contexts owned by the Rust wrapper, which charges every chunk, including its header, to the caller's resource hooks before allocating and releases it after freeing. A refused reservation or failed allocation returns NULL, so the engine reports `REG_ESPACE`. Requests above `MaxAllocSize` and overflowing array sizes raise the same errors as PostgreSQL's memory-context code.
- `CHECK_FOR_INTERRUPTS()`, which the imported `INTERRUPT()` macro uses, and `stack_is_too_deep()`, which backs `STACK_TOO_DEEP()`, call the caller's cancellation and stack-depth hooks. Stack depth is measured from the entry of the current engine operation. Beyond PostgreSQL's default `max_stack_depth` of 2 MB, or a lower limit of the hooks, the engine reports `REG_ETOOBIG` as in PostgreSQL. Every operation runs with stack for that depth and PostgreSQL's `STACK_DEPTH_SLOP` of 512 KiB beyond it: on the thread's own stack when it has that much, and otherwise on a separately allocated stack of the same thread through the `stacker` crate. An operation whose thread lacks that headroom first runs on the thread's stack while its depth leaves the slop free; a deeper `stack_is_too_deep()` call abandons it through the operation's error frame, and the wrapper runs it again on a separate stack. Under Emscripten the engine's calls run on the WebAssembly call stack, which can be neither measured nor switched, and operations run where they are.
- Under Emscripten the build compiles `regcomp.c` and `regexec.c` with `-finstrument-function-entry-bare` and `-fstack-protector-all`. The first calls `__cyg_profile_func_enter_bare()`, which the shim defines, on entry to every engine function that remains a function after inlining; the second gives each of them a frame of its own on the stack in linear memory, so a frame entered at or above a recorded position shows that the recorded frame has returned. The shim thus counts the thread's live engine frames exactly, restores the count when an operation leaves, and reports it at each depth check, where the Rust wrapper charges each frame a fixed depth calibrated so that `MAX_STACK_DEPTH` corresponds to half of V8's default stack at the largest engine frame measured. Every compilation and execution therefore fails with `REG_ETOOBIG` before it can exhaust the call stack. Native builds are not instrumented.
- PostgreSQL leaves the regular expression code through `ereport(ERROR)` and a `longjmp` to its error handler. The shim keeps that structure entirely in C: every engine call runs inside a UQA-owned C function that establishes a `sigsetjmp` frame without saving the signal mask, selected as PostgreSQL's `c.h` selects it (plain `setjmp` on Windows and the compiler builtins on MinGW-w64), and cancellation, panics of the Rust hooks, a stack too small for the operation and allocation-size errors jump only to that frame, never across Rust frames or between stacks. The Rust wrapper then frees every chunk of the abandoned operation, as PostgreSQL resets the memory context of a failed query.
- A compiled expression is freed by deleting its memory context, as `RE_compile_and_cache` deletes the context of an expression it discards, so `pg_regfree` is compiled but not called.
- `src/backend/regex/regc_pg_locale.c` is not imported. `regcomp.c` includes `shim/regex/regc_pg_locale.c` instead, a port that keeps the selected locale strategy in the per-thread engine frame rather than in process-global variables, so concurrent operations on different threads are independent. It implements PostgreSQL's C, builtin and ICU strategies: the C strategy uses PostgreSQL's ASCII property table, the builtin strategy calls the imported `unicode_category.c` and `unicode_case.c` with the provider's `casemap_full` setting, and the ICU strategy evaluates ICU4C's `u_is*`, `u_toupper` and `u_tolower` definitions over ICU4X Unicode 16 data. The libc strategies are not provided because UQA defines no libc collation other than C and POSIX. Character-class cvecs, which PostgreSQL caches per backend in malloc'd memory, are kept complete once per process in immutable Rust storage, so a class reference allocates nothing, as a hit in PostgreSQL's cache does.
- The engine's `Oid collation` argument carries a UQA locale token chosen by the Rust wrapper after it has applied PostgreSQL's collation checks, so the `ereport` paths of PostgreSQL's `pg_set_regex_collation` are rejected before C runs.
- Global symbols defined by the imported files and the shim are renamed with a `uqa_pg_regex_` prefix through macros in `shim/include/postgres.h`, so they cannot collide with the PostgreSQL parser objects that `uqa-pg-query` links into the same binary.

## Licenses

The imported files keep their notices: `postgres/COPYRIGHT` is the PostgreSQL License, and `postgres/src/backend/regex/COPYRIGHT` carries Henry Spencer's license for the regular expression package and the Tcl license for portions of `regc_locale.c`. `LICENSE` states the terms of the UQA-owned wrapper, build script and shims.
