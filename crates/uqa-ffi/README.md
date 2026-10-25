# UQA C bindings

This crate implements the embedded C ABI described in the [C binding design](../../docs/design/c-bindings.md). Implementation and qualification are in progress; the [implementation ledger](../../docs/plans/0028-c-bindings.md) records the complete scope and remaining work. The checked-in headers are not a released interface yet.

The Cargo library target is `uqa_ffi`, avoiding a collision with the existing Rust `uqa` facade. The native distribution installs the library as `uqa` and includes headers under `include/uqa`. CMake links the Cargo static archive with the platform's exact public-symbol list. Final platform and release qualification remains in the implementation ledger.

Host logging is deferred at the maintainer's request. `uqa_log.h` retains interface declarations only; their functions are not implemented or exported. This delivery adds no native logging or callback adapter.

All public functions return `uqa_status`. Opaque handle typedefs include their pointers, strings use C strings or explicit-length variants, diagnostics are borrowed from the calling thread, and acquisition/release names stay paired. SQL execution accepts native multi-statement and empty messages.

Open every backend with `uqa_engine_open(path, flags, config, &engine)` and close it with `uqa_engine_close(&engine)`. Choose exactly one storage selector: `UQA_OPEN_MEMORY` with a NULL path, `UQA_OPEN_SQLITE_NATIVE` for native SQLite, `UQA_OPEN_SQLITE_KV` for SQLite Key/Value, or `UQA_OPEN_REDB`. Either SQLite layout also accepts `UQA_OPEN_SQLITE_AUTO` to detect file encryption/compression. Memory and redb reject SQLite modifiers and encryption/compression settings. Persistent Engines support independent siblings through `uqa_engine_open_session`. See the [C source API reference](../../docs/manual/reference/08-bindings-and-extensions.md#c-source-api).

Both SQLite layouts support trusted anchors through `uqa_config_set_trusted_anchor` and `uqa_config_get_trusted_anchor`. Extract the next anchor with `uqa_sqlite_get_container_anchor` after closing all writers, and keep its fields in independently trusted storage. Anchored opens require encryption and explicit or detected compression; anchored AUTO never creates a missing file. The native storage owner validates the identity, generation and authentication tag.

Scalar constructors preserve SQL integer and floating widths. Exact NUMERIC and distinct JSON/JSONB constructors use native SQL input, with C strings or `_n` spans; matching getters return borrowed text without implicit conversion. Values expose borrowed type descriptors, including declared numeric modifiers, while numeric display scale belongs to the value. Vector and tensor inputs copy finite float32 elements with a positive dimension; empty tensors retain their dimension. Matching getters return stable borrowed packed views and distinguish these types from SQL arrays. Array shape and element getters preserve signed bounds, row-major order and typed NULL children without flattening the native array. See the [value contract](../../docs/design/c-bindings.md#10-values-and-exact-representations) for lifetimes and the remaining representation families.

Contextual `uqa_value_create_typed_null` and `uqa_value_create_from_text` constructors resolve native type declarations with modifiers and retain the creating session. Domain input and its routine effects execute within the existing statement/transaction boundary, including failure and savepoint recovery. Both constructors have `_n` variants and return values released by `uqa_value_destroy`.

Execution resolves retained catalog parameter identities in the target session, including NULL and unused inputs, empty SQL and transaction commands. Dropped declarations report native SQLSTATE `XX000`; array diagnostics retain the array OID. Copies and NULL inspection preserve deferred observation. Native transaction admission rejects parameterized requests in a failed frame, while parameterless rollback and savepoint recovery remain available.

UUID constructors accept native text or sixteen network-order octets, with separate borrowed byte and canonical-text getters. BIT and VARBIT constructors copy packed MSB-first bytes and a logical bit length; their separate getters preserve empty values, partial final bytes and declared type identity. SQL owns canonicalization and checked retention. The same APIs read stored and contextual values, including domain bases.

Reusable UDF options use matching create/destroy and get/set functions. The fixed volatility codes are `UQA_VOLATILITY_IMMUTABLE`, `UQA_VOLATILITY_STABLE` and `UQA_VOLATILITY_VOLATILE`; `may_mutate_engine` accepts 0 or 1. Defaults follow native registration: VOLATILE and may-mutate. Invalid setters preserve the current value, and options remain independent of Engine lifetime. Register scalar, streaming table and aggregate functions with `uqa_udf_register_scalar`, `uqa_udf_register_table` and `uqa_udf_register_aggregate`. Table registration copies its ordered column names before any scan, so empty and correlated sources have a fixed descriptor. Each returns a shared registration handle released by `uqa_udf_unregister`; busy retirement preserves the handle, while completed retirement clears it even when user-data destruction fails. Registrations do not retain their creating session. Callback arguments and contexts belong to the invoking session, and cleanup runs once after successful scan/state creation, including on early stop, failure and cancellation. Every callback returns `uqa_status`; no exception or `longjmp` may cross the ABI.

Enable `arrow-export` for `uqa_result_export_arrow` and `uqa_cursor_fetch_arrow`; the header includes the standard Arrow C Data Interface. `UQA_ARROW_LOSSLESS` uses declared exact mappings and rejects unqualified types or values. `UQA_ARROW_SQL_TEXT` explicitly selects SQL presentation text. Schema and array releases are independent, and moved top-level structs or detached children retain their owners after result/cursor/Engine destruction. The array allowance remains charged until its last retained array child is released; keeping schema metadata alone does not retain the data allowance. Export failure leaves empty outputs, and a conversion failure after a cursor fetch is terminal. Without the feature, both symbols return `UQA_ERROR_UNSUPPORTED`; Arrow export does not enable either foreign-data wrapper.

`uqa_value_create_array` and its `_n` variant copy explicit dimensions and row-major child values into a native typed array. Scalar casts and domain constraints run in the native transaction scope, and array-valued domains remain individual elements through formatting, result conversion and persistence.

`uqa_value_create_record` and `_n` copy fields into named composite/domain or anonymous record parameters. Named fields use native casts and domain checks; anonymous fields retain their declared metadata. `uqa_type_get_field_count` and `uqa_type_get_field` expose live named-composite attributes, with stable borrowed names/types and bounded admission under their parent owner.

`uqa_value_create_range` and `uqa_value_create_multirange`, with `_n` variants, construct typed ranges through native endpoint conversion, canonicalization, normalization and domain checks. Bound and member getters return parent-owned views under the original allowance, with separate empty, unbounded and present-infinite states. Type getters expose the endpoint and associated range descriptors. Read-only observation does not repeat catalog input, domain checks or canonical functions.

Run `cargo test -p uqa-ffi --no-default-features --test integration` for the current source ABI suite. On Unix, its single Rust harness compiles and executes the same nineteen native fixtures as C11 and C++17 clients against all four backends. Persistent cases also verify sibling isolation, savepoint recovery, retained subscriptions, exact scalar and packed-value storage, restored catalog type input, close/reopen, rollback on close and SQLite encryption/compression. `scripts/verify-c-package.py` compiles the installed CMake shared/static targets and pkg-config consumers, including Windows native synchronization and spill cleanup. Additional platforms, feature combinations and release qualification remain tracked in the implementation ledger.

## Native builds and installed consumers

Source builds require Rust, CMake 3.24 or later, Python 3.11 or later and the target's C/C++ toolchain. `scripts/build-c-bindings.py` compiles the selected native target, obtains its actual static-link dependencies from rustc, links the restricted shared library and installs both libraries, headers and metadata. Select the native target triple for your host. On macOS, set `MACOSX_DEPLOYMENT_TARGET` before compiling Rust or native dependencies; distribution CI uses 11.0. Windows uses the MSVC ABI and dynamic CRT (`/MD`).

```sh
MACOSX_DEPLOYMENT_TARGET=11.0 python3 scripts/build-c-bindings.py \
  --target aarch64-apple-darwin \
  --prefix target/c-installed --build-directory target/c-native-build
python3 scripts/verify-c-package.py --prefix target/c-installed
```

The build defaults to `nori,kuromoji`. `--features ""` selects the minimal library; `--features nori,kuromoji,arrow-export` adds Arrow export. `arrow` and `duckdb` select their independent native foreign handlers. The source build script uses the development profile by default. Release CI selects the release profile and qualifies the actual installed libraries before archiving them.

Use `find_package(UQA CONFIG REQUIRED)` with the installation prefix in `CMAKE_PREFIX_PATH`, then link one target:

```cmake
find_package(UQA CONFIG REQUIRED)
add_executable(application application.c)
target_link_libraries(application PRIVATE UQA::uqa)
```

`UQA::uqa` propagates `UQA_USE_SHARED`; `UQA::uqa_static` selects the exact static archive and its native dependencies without a shared-consumer macro. The pkg-config equivalents are `uqa` and `uqa-static`. Use `pkg-config --cflags --libs uqa-static` for the static archive. Windows pkgconf supports `--msvc-syntax`; its compile flags belong before the MSVC `/link` separator and its library flags after it.

Packages contain `include/uqa`, libraries, `lib/cmake/UQA`, `lib/pkgconfig`, and `share/uqa`. `share/uqa/uqa-build.json` identifies product version, ABI major, source revision, enabled features, exact exports, deferred logging declarations, native link requirements, build runtime baseline and dependency license digests. Runtime library identity is checked against that manifest. Shared libraries use ABI major 1 independently of the product version. Windows keeps `uqa.lib` for the DLL import library and `uqa_static.lib` for the static archive.

The automatic C package workflow targets Linux x86_64/aarch64, macOS x86_64/arm64 and Windows x86_64. It relocates the installation into a path containing spaces, compiles every installed header in all three linkage modes, rejects conflicting modes, checks the final native exports, and executes every ABI fixture and public example as C11/C++17 with shared and static linkage. Release publication requires all five target artifacts and verifies their source identity, complete file inventory, checksums, licenses and embedded dictionaries. Logging stays declaration-only. Configuring these gates does not establish that a pending candidate has passed them.
