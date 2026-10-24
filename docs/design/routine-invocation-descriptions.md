# Routine invocation descriptions

Related issue: [#1108](https://github.com/cognica-io/uqa-engine/issues/1108). The [implementation and acceptance ledger](../plans/open-performance-corrections.md#scoped-routine-invocation-descriptions) records delivery separately from this contract.

## Ownership and scope

SQL owns the complete function binding, selected catalog function and resolved assignment descriptions. Execution owns their lifetime in the existing prepared-expression state, invocation scheduling and memory admission. Native Engine adapters supply catalog revision, weak routine-registry identity and the invoking role's OID and incarnation. They do not select an overload or implement conversion. The public EngineHook extension defaults to its original bound-call method, so custom adapters retain live behavior unless they explicitly implement scope validation.

A reusable binding has a stored function identity, complete supplied-source and argument mappings, and fixed source, target and parameter names: catalog identities or parsed two-part names. Unqualified names and legacy incomplete bindings keep ordinary resolution. The complete binding is compared before a description is read; a syntax address alone never establishes semantic identity. Execution indexes slots by the owned invocation allocation, which stays distinct for zero-argument calls and stable when the prepared owner moves. The original binding remains the source of the concrete invocation mapping.

The native scope retains a weak reference to the selected routine registry, and the description holds a weak reference to its selected function. A retired function body is not kept alive by optional invocation metadata; a failed weak upgrade uses ordinary selection. A registry replacement or copy-on-write mutation cannot reuse that weak allocation identity; catalog revision and the complete role identity must also agree. The scope does not keep an entire superseded registry alive. Qualified and identity-bound type names avoid dependence on search_path; conversions still read current settings and callbacks. A changed scope declines reuse for the remaining lifetime of that description rather than accumulating catalog generations.

## Evaluation order

The first invocation follows the existing order: resolve supplied source types, select the bound routine with SQL's original signature/kind checks, then convert arguments and evaluate defaults in their established order. A successful selection may retain its identity and source descriptions. Each target description is selected lazily at that target's original conversion boundary. Failed lookup or conversion does not publish a target description.

Every later invocation evaluates its SQL operands before consulting the retained state. Each argument source and target read rechecks the native scope after preceding conversions, since domain/input callbacks can execute SQL. A conversion publishes its newly selected description only if that scope still agrees afterward. No cache lock is held across user code. Changing values, domain membership, defaults, variadic construction, strictness, EXECUTE privilege checks, security/settings guards and routine bodies remain live. A stale description falls back at the same point, without reevaluating an operand or replaying a callback.

The correction retains metadata, not results. SQL-language body and return conversion paths continue to own their existing declared-result checks; retaining an invocation description alone does not establish that all routine execution metadata or row-time name resolution has been eliminated.

## Preservation argument

Let $B$ be the complete immutable binding, $C$ the selecting catalog/registry/role scope, $V$ the current input values, and $H$ the ordered live callbacks and session state. Write ordinary invocation as $I(B,C,V,H)=E(R(B,C),V,H)$, where $R$ resolves supplied types and selects the function and argument mapping, and $E$ performs the original conversions, defaults, authorization and body execution. Under the adapter's identity contract and fixed names, $R(B,C)$ is invariant. Therefore replacing repeated $R(B,C)$ by its retained value leaves $I$ unchanged while $B$ and $C$ agree.

For each argument position $i$, let $D_i(C)$ be its target assignment description and $A_i(D_i,V_i,H_i)$ the existing assignment kernel. Metadata is first selected at the original position, and the current scope is checked after all earlier effects. If it agrees, $D_i(C)$ is identical to the original lookup and $A_i$ receives the same value, source description and live callbacks. If it differs, the ordinary lookup runs at that position. Induction over the ordered argument/default positions preserves values, errors and effects, including a callback that invalidates a later position. Publication after successful conversion cannot change an earlier observation, and post-callback scope validation prevents stale metadata from entering the retained state.

The final authorization and execution steps use the original selected function and current authority. Metadata reuse neither treats strict NULL input as permission to skip EXECUTE checks nor makes a function replacement or rollback invisible. The SQL bag carrier, multiplicity, NULL behavior, result types and UnifiedPlan structure are unchanged; no new algebraic operator or rewrite rule is introduced.

## Resource and work contract

There is at most one optional description per prepared logical call, independent of input row count. SQL reuses the scalar lowerer's admitted complete-binding copy. The header, source vector, lazy target slots and copied type payloads retain their original production reservations; no independent cache budget is introduced. Insufficient optional allowance declines retention, while cancellation and malformed metadata remain errors. Dropping the prepared owner releases its leases, and cloning prepared expressions creates independent empty state.

Reuse eliminates repeated registry traversal and type-name resolution for the eligible invocation, while complete-binding comparisons and constant-size native scope checks still occur. It does not cache converted values or remove per-value domain checks. Deterministic work counters and exact output/resource assertions qualify this claim separately from controlled elapsed-time measurements and the original historical benchmark obligations.
