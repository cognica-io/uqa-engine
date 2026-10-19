# Resolved numeric cast execution

[#1108](https://github.com/cognica-io/uqa-engine/issues/1108) identifies repeated textual type resolution in row evaluation. SQL owns `NumericCast`, selected only from resolved builtin integer, floating-point and numeric identities. It invokes the existing conversion and numeric modifier kernels directly. Execution owns one optional descriptor per immutable prepared cast expression and retains its already-admitted source name; Engine supplies the invoking catalog callbacks.

The first evaluated row follows the original source inference, operand evaluation, catalog resolution and conversion order. Only a successful conversion to an explicitly qualified builtin target is retained. Subsequent rows of the same type schema evaluate the operand once and use that descriptor without name construction, parsing or catalog name lookup. A changed schema follows the original path; cloned prepared expressions start with independent empty state. Unqualified source or target names, parameter-dependent operands, domains, OID aliases, containers and session-dependent casts retain their original resolution paths.

The retained object contains conversion metadata, never the previous row's result. Every invocation keeps its current domain adapter, cancellation tokens and output allowance. Retained source-name capacity stays charged to the original production owner and is released when the prepared expression dies. This does not cache a domain check or remove an operand evaluation.

## Preservation argument

The selected builtin source and destination identities determine the numeric kernel and destination modifier; neither reads search_path, role, timezone, DateStyle or a mutable catalog descriptor. Reusing that selection for the same immutable expression and schema is equivalent to selecting it again. The value conversion itself remains per row, so rounding, overflow, NULLs, output allocation failure and the invoking callback's value or error remain observable at their original evaluation point. Expressions that cannot establish this invariant use the existing generic conversion.

This optimization introduces no new algebraic operator and does not reorder, eliminate or duplicate SQL operand evaluation. It preserves numeric results and SQL error order; it changes only repeated metadata work and the bounded lifetime of an admitted type name.

## Verification and remaining work

Local tests exercise 10,000 rows with exactly one cast resolution and two catalog name lookups, independent expression clones, changed schemas, unqualified source/target names, changing parameter types, callback errors, zero scratch, cancellation, rounding, typmods and overflow. The numeric expectations were checked against PostgreSQL 18.4 in Docker. These owner cases are registered in the deterministic performance inventory.

This is one bounded correction of #1108. General operator/routine descriptor reuse, nonnumeric conversions, remaining repeated statement/block analysis tracked in #1117 and the original controlled historical TPC-H comparison remain required. No new timing acceptance or complete issue resolution is claimed by these deterministic checks. The PR records the exact tested source and actual Linux/macOS regression results before delivery.
