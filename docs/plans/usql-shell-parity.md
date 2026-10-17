# usql shell correctness and command parity

The complete shell correction is delivered in one PR at the maintainer's request, with logical commits. The CLI owns editing, command interpretation and presentation; Engine retains SQL, transactions, catalog, COPY and cancellation semantics. No HTTP server or transport is introduced.

## Original task and live progress

The requested comparison found reproducible valid-input rejection, literal/comment highlighting and completion errors, ignored timing/expanded arguments, malformed Unicode tables, raw terminal controls and missing psql shell commands. All of the following remain acceptance requirements; an implemented item is not complete until its tests and delivery evidence are recorded.

- [x] Accept quoted brackets and preserve incomplete SQL context across lines; handle quoted identifiers, nested comments, escape/dollar strings, matching brackets, NO_COLOR and HIGHLIGHT.
- [x] Complete actual catalog names, unqualified/qualified/quoted names, columns, variables, settings and paths without completing inside strings or comments.
- [x] Implement explicit timing and expanded on/off/auto, pset formats and aliases, display-width alignment, terminal escaping, duplicate columns and format-preserving file output (including CSV NULL settings and native COPY).
- [x] Implement query-buffer commands g/gx/gset/p/r, variables and conditionals, error policy and autocommit without changing simple-query transaction boundaries.
- [x] Implement bounded, cycle-safe i/ir/run scripts, history controls/save, editor, output files and pager.
- [x] Implement native file COPY, watch, database reconnection/connection information, cancellation and notification draining through existing Engine interfaces.
- [x] Correct catalog command meanings, patterns and SQL help while retaining explicit UQA graph/analyzer/foreign-server commands.
- [x] Verify public executable behavior against independent PostgreSQL 18 references and owning-crate tests; update the manual, history and command help.
- [ ] Review actual automatic Linux/macOS performance results and applicable final-ready qualification; merge and remove the completed branch.

## Preservation argument

Let $S$ be Engine session state, $Q$ the sequence of submitted SQL strings and $E(S,Q)$ its ordered observations and final state. Lexing, highlighting and input editing operate on text without invoking $E$; removing inserted ANSI styling must return the exact original text. Completion changes only the user-selected input span. These text transformations preserve $E$ for every unchanged submitted $Q$, including UQA document, graph and ranked carriers. Catalog completion obtains names with a read-only query through the current session; its catalog visibility and read-lock effects follow the existing Engine semantics. It does not issue a query while the transaction is aborted, leaving rollback available.

Script interpretation maps shell text and shell variables to an ordered $Q$. Execution delegates each submitted SQL unit to the existing Engine adapter. Concatenation of script units corresponds to stateful composition $E(E(S,Q_1).\mathrm{state},Q_2)$, with explicit SQL transaction boundaries retained. The existing simple-query command adapter remains responsible for atomic multi-statement segments. Conditionals remove only inactive shell units, and variable substitution operates only in lexically eligible tokens; neither rewrites Engine operators.

More formally, let $T_Q : S \to S \times O^*$ return session state and the ordered Engine observations, including errors. Define stateful composition by $(T_B \star T_A)(s)=(s_B,o_A \mathbin{+\!+} o_B)$ where $T_A(s)=(s_A,o_A)$ and $T_B(s_A)=(s_B,o_B)$. Empty input has $T_\epsilon(s)=(s,[])$, giving both identity laws. Expanding either parenthesization of three submissions yields $(s_C,o_A \mathbin{+\!+} o_B \mathbin{+\!+} o_C)$, so associativity follows from ordered-sequence concatenation. The shell invokes the same existing adapter for each submission and never reassociates transactional segments; induction over submitted units therefore preserves state and observations. A result-dependent conditional or variable selects the next unit from the already observed prefix, and ON_ERROR_STOP terminates that prefix without introducing a new Engine operation.

Result rendering maps the existing ordered typed column/row observations to presentation bytes without changing $S$ or $Q$. Positional duplicate columns, NULL and exact type formatting are retained before presentation. Terminal escaping is a presentation-only mapping; file CSV/COPY preserves data according to the selected format. COPY, cancellation and notifications use the existing session interfaces rather than synthesizing SQL data or transaction outcomes. These boundaries introduce no new algebraic operator or optimizer rewrite, so all existing carrier operations and composition laws remain those of $E$.

The bundled PL/pgSQL extension projection exposes an already implemented language through SQL-owned identities and Execution-owned catalog rows. Its row, relation, attribute, type and dependency identities are compared with PostgreSQL 18. This adds catalog observations without changing existing carrier operations, language execution, catalog write transactions or extension installation. Privilege display reads existing relation ACLs rather than synthesizing grants.

## Evidence

Baseline source: main `1b3557fa8`. Reproduced before implementation with the native executable: quoted opening bracket blocks Enter, catalog prefix completion fails, string/comment completion inserts SQL keywords, timing/expanded off toggles on, and Unicode/control output is malformed. The complete input, rendering and client-command correction is implemented in this PR.

Focused local verification passes 57 CLI owner tests, 36 binary integration tests, 105 Execution catalog tests and two SQL catalog identity tests. The real Unix PTY fixture checks Enter, multiline/literal highlighting, qualified and unqualified Tab completion, Ctrl-C cancellation, aborted-transaction rollback and session reuse, NO_COLOR/HIGHLIGHT, idle cross-process notification delivery, editor, pager and persisted history. Owner tests additionally cover script boundaries, native file COPY, finite watch, rollback-aware subscriptions, output formats and the existing simple-query transaction contract.

PostgreSQL 18.4 Docker references establish the shell fixture, typed CSV output and bundled PL/pgSQL metadata. Full catalog-command probing reproduced #995 and the privilege command's absent-view query; the corrections now pass through the actual executable. Final local review also removes per-character escape allocations, redundant CSV row copies and repeated function-registry collection, reuses SQL-owned identifier quoting, and verifies PostgreSQL non-ASCII variable names and inert completion labels. Strict Clippy with warnings denied passes all targets of CLI, SQL and Execution. Formatting, dependency direction, capability ownership, integration harnesses, file headers/size, public repository hygiene and release-license source checks pass.

The final unchecked item is automatic Linux/macOS performance qualification, applicable ready-for-review FDW qualification, PR review and merge/cleanup. Successful compilation alone does not satisfy it; record the tested source and actual work/memory/output results before delivery.
