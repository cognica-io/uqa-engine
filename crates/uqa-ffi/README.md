# UQA C bindings

This crate implements the embedded C ABI described in the [C binding design](../../docs/design/c-bindings.md). Implementation and qualification are in progress; the [implementation ledger](../../docs/plans/0028-c-bindings.md) records the complete scope and remaining work. The checked-in headers are not a released interface yet.

The Cargo library target is `uqa_ffi`, avoiding a collision with the existing Rust `uqa` facade. Planned native distribution packages will install the library as `uqa` and include the headers under `include/uqa`.

All public functions return `uqa_status`. Opaque handle typedefs include their pointers, strings use C strings or explicit-length variants, diagnostics are borrowed from the calling thread, and acquisition/release names stay paired. SQL execution accepts native multi-statement and empty messages.
