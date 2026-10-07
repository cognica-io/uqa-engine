# Ordinary statement analysis reuse

This optimization addresses the repeated statement-analysis cost in #345. SQL owns input conversion and parameter-shape eligibility, Planner always optimizes against current inputs, Execution chooses the pinned statement boundary, and Engine retains entries in its existing 256-statement session cache. It introduces no SQL syntax, isolation mode, durable format or algebraic operator.

## Reuse boundary

Only a single ordinary statement can use this entry. Nested routine/rule execution and multi-statement batches retain their existing paths. Execution asks for the entry after snapshot pinning and every writer refresh. Engine validates the original SQL/parser entry and catalog/registry epochs, and declines analysis reuse while private catalog or registry changes are dirty. Existing namespace, role, parser and restored-catalog invalidation also clears the entry.

SQL retains the analyzed, unoptimized tree and each parameter's inferred SQL type and explicit-declaration flag. Values are supplied by every invocation and are never stored in the entry. Parameter inference includes the current array/vector shape, temporal type, declared domain/composite type and other type information used by binding. Inference failure only disables reuse; the original analyzer still controls diagnostic ordering. A string supplied without a declaration remains distinct from declared text and from NULL.

The existing input-conversion classifier rejects conversions whose meaning changes between ordinary messages, including temporal special inputs, catalog-dependent inputs and composite/domain-array input. Such statements still analyze at every message. The cache does not extend the creation-time lifetime of PREPARE into ordinary SQL.

## Preservation argument

Let $A(S, C, P, I)$ denote analysis of syntax $S$ under binding context $C$, parameter type/declaration shape $P$ and input-conversion environment $I$. Let $O(A, D)$ optimize the analyzed tree using current data/statistics $D$, and let $E(O, V, T)$ execute with current parameter values $V$ and transaction state $T$.

An eligible cache hit requires unchanged $S$, the same validated catalog, namespace, authority and parser context $C$, and equal $P$. Every retained input conversion is immutable across ordinary messages, so its output is independent of a later $I$. Ordinary binding observes parameter types/declarations, while parameter values remain expressions evaluated by execution. Thus the retained tree equals $A(S, C, P, I)$ for the new invocation. A mismatch or unavailable proof performs the original analysis instead.

Planner still computes $O$ using the new $D$ on every invocation, and execution still receives the new $V$ and $T$. Therefore the resulting execution is $E(O(A(S,C,P,I),D),V,T)$ on both paths. No row, posting, graph, score or probability carrier is replaced or combined differently; all downstream UQA operators receive the same analyzed expressions, current physical plan and invocation inputs. Their existing algebraic laws and observations are preserved by this identity of inputs.

The snapshot boundary is essential to the argument. Looking up analysis before pinning could miss a catalog commit whose process epoch has not yet been published. Refresh invalidates the session entry before the lookup used here. Private DDL disables reuse, rollback restores existing session state, and later external definitions force analysis again. Unchanged data alone does not alter $A$, but still changes the inputs of $O$ and $E$.

## Verification

SQL-owner tests distinguish inferred and declared parameter types and retain the ordinary temporal-input lifetime and diagnostic-order checks. Engine tests exercise repeated parameterized INSERTs on memory, native SQLite, SQLite Key/Value and redb, verify one retained analysis owner across changing values, and cover rollback, incompatible parameter types, private DDL and external schema changes. Existing PostgreSQL reference fixtures cover input-conversion order and statement/transaction clocks. The automatic work inventory checks reuse and SQL outcomes; it does not claim an elapsed-time speedup.
