# usql shell correctness and command parity

The complete shell correction is delivered in one PR at the maintainer's request, with logical commits. The CLI owns editing, command interpretation and presentation; Engine retains SQL, transactions, catalog, COPY and cancellation semantics. No HTTP server or transport is introduced.

## Original task and live progress

The requested comparison found reproducible valid-input rejection, literal/comment highlighting and completion errors, ignored timing/expanded arguments, malformed Unicode tables, raw terminal controls and missing psql shell commands. All of the following remain acceptance requirements; an implemented item is not complete until its tests and delivery evidence are recorded.

- [ ] Accept quoted brackets and preserve incomplete SQL context across lines; handle quoted identifiers, nested comments, escape/dollar strings, matching brackets, NO_COLOR and HIGHLIGHT.
- [ ] Complete actual catalog names, unqualified/qualified/quoted names, columns, variables, settings and paths without completing inside strings or comments.
- [ ] Implement explicit timing and expanded on/off/auto, pset formats and aliases, display-width alignment, terminal escaping, duplicate columns and lossless file output.
- [ ] Implement query-buffer commands g/gx/gset/p/r, variables and conditionals, error policy and autocommit without changing simple-query transaction boundaries.
- [ ] Implement bounded, cycle-safe i/ir/run scripts, history controls/save, editor, output files and pager.
- [ ] Implement native file COPY, watch, database reconnection/connection information, cancellation and notification draining through existing Engine interfaces.
- [ ] Correct catalog command meanings, patterns and SQL help while retaining explicit UQA graph/analyzer/foreign-server commands.
- [ ] Verify public executable behavior against independent PostgreSQL 18 references and owning-crate tests; update the manual, history and command help.
- [ ] Review actual automatic Linux/macOS performance results and applicable final-ready qualification; merge and remove the completed branch.

## Preservation argument

Let S be Engine session state, Q the sequence of submitted SQL strings and E(S,Q) its ordered observations and final state. Shell editing operates on text without invoking E; removing inserted ANSI styling must return the exact original text. Completion changes only the user-selected input span. Therefore both operations preserve E for every unchanged submitted Q, including UQA document, graph and ranked carriers.

Script interpretation maps shell text and shell variables to an ordered Q. Execution delegates each submitted SQL unit to the existing Engine adapter. Concatenation of script units corresponds to stateful composition E(E(S,Q1).state,Q2), with explicit SQL transaction boundaries retained. The existing simple-query command adapter remains responsible for atomic multi-statement segments. Conditionals remove only inactive shell units, and variable substitution operates only in lexically eligible tokens; neither rewrites Engine operators.

Result rendering maps the existing ordered typed column/row observations to presentation bytes without changing S or Q. Positional duplicate columns, NULL and exact type formatting are retained before presentation. Terminal escaping is a presentation-only mapping; file CSV/COPY preserves data according to the selected format. COPY, cancellation and notifications use the existing session interfaces rather than synthesizing SQL data or transaction outcomes. These boundaries introduce no new algebraic operator or optimizer rewrite, so all existing carrier operations and composition laws remain those of E.

## Evidence

Baseline source: main `1b3557fa8`. Reproduced before implementation with the native usql executable: quoted opening bracket blocks Enter, catalog prefix completion fails, string/comment completion inserts SQL keywords, timing/expanded off toggles on, and Unicode/control output is malformed. Implementation and qualification are pending.
