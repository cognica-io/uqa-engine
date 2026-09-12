# Analyzer Pipeline Internals

Analyzer behavior crosses analysis, storage, engine catalog, SQL execution, and retrieval operator boundaries. This chapter identifies the owning representations and the invariants required to keep indexed and queried vocabularies compatible.

## Ownership map

| Concern | Owner | Primary representation |
| --- | --- | --- |
| Pipeline stages and validation | `uqa-analysis` | `Analyzer`, `CharFilter`, `Tokenizer`, `TokenFilter` |
| Source mapping and token graph | `uqa-analysis` | `FilteredText`, `TextCoordinates`, `AnalysisToken`, `AnalyzedText` |
| Built-in and process-global registry | `uqa-analysis::registry` | Immutable built-ins plus a process-global custom map |
| Persistent named definitions | `uqa-engine` and `CatalogFacade` | Analyzer name to JSON configuration |
| Persistent field assignment | `uqa-engine` and `CatalogFacade` | Table, field, normalized phase, analyzer name |
| Index and search analyzer instances | `InvertedIndex` implementations | Per-field index and search analyzer maps |
| SQL lifecycle | `uqa-execution::query::table_functions::analyzers` | Mutating table functions and `fts_index_stats` |
| Query-time resolution | `uqa-operators` and engine search paths | `get_search_analyzer(field)` |

The engine catalog stores JSON and names, while inverted-index instances hold cloned, validated `Analyzer` values. A definition update therefore does not mutate every installed clone automatically; an owning GIN definition must be recreated or a field assignment must be reapplied.

## Analysis execution

```mermaid
flowchart LR
    A[Source string] --> B[CharFilter 1]
    B --> C[CharFilter N]
    C --> D[Tokenizer]
    D --> E[TokenFilter 1]
    E --> F[TokenFilter N]
    F --> G[AnalyzedText with source and position state]
    G --> H[Ordered term projection for existing consumers]
```

`Analyzer::analyze_tokens` carries `FilteredText` through the character-filter loop, tokenizes once, and moves `AnalyzedText` through the token filters. Character edits map filtered UTF-8 and UTF-16 coordinates back to the original input; token filters preserve source spans, graph increments and lengths, keyword state, and final skipped positions. `Analyzer::analyze` projects this canonical result into ordered terms. Every stage is fallible. An empty stream is a valid result; an invalid regular expression, invalid gram range, failed synonym-file read, or position overflow is an error and must not be converted into an empty result.

Generic terms use `TokenTerm`, with canonical scalar strings and explicit raw UTF-16 storage for unpaired units. Checked string projection fails for a non-scalar term. Nori streams convert directly into this representation, retaining optional Korean morphology and pre-correction coordinates while projecting exact original UTF-16 and safe UTF-8 source ranges. An opaque terminal token preserves attribute changes when an upstream filter is exhausted; subsequent filters must retain that state independently of the emitted token list. The native bridge is exercised against all 803 successful text cases across the tokenizer, analyzer, and number corpora.

Removal filters accumulate skipped increments, including trailing removals. Synonym and gram-filter expansions share the input position and length; tokenizer grams occupy consecutive positions. Unchanged substrings preserve precise source offsets, while rewritten terms retain covering spans. Existing storage and query consumers still call the term-only API; graph occurrence storage and phrase matching remain tracked in the [Nori implementation plan](../../plans/0006-nori-analyzer.md). The [Rust reference](../reference/06-text-analyzers.md#structured-tokens) defines the public metadata contract.

Configuration uses Serde tagged enums. Most serialized tags derive from Rust variant spelling, including `n_gram` for `Tokenizer::NGram` and `ngram` for `TokenFilter::Ngram`. Acronym-bearing variants have explicit stable names: `html_strip` for `CharFilter::HTMLStrip` and `ascii_folding` for `TokenFilter::ASCIIFolding`. Deserialization also accepts the derived spellings `h_t_m_l_strip` and `a_s_c_i_i_folding` that releases up to 0.1.2 persisted, so catalogs written before the stable names still open; serialization always emits the stable names. Engine parsing normalizes string shorthand only for the tokenizer and token-filter arrays; canonical object tags remain the compatibility contract.

## Definition validation

`parse_analyzer_config` performs four steps:

1. Parse the source as JSON.
2. Normalize supported string shorthand into tagged objects.
3. Deserialize an `Analyzer`.
4. Call `Analyzer::validate` before catalog publication.

Validation compiles pattern tokenizers and pattern-replacement character filters, checks positive ordered gram bounds, and reads a configured synonym file. Uncompiled execution repeats fallible checks. Index providers compile a revision before installing it and retain its resolved inputs; subsequent execution of that handle does not reread synonym files. Legacy catalog restoration still validates named configuration inputs.

## Analyzer resolution

`Engine::resolve_analyzer` trims and rejects an empty name, then resolves in this order:

1. The process-global `uqa_analysis` registry, including built-ins.
2. The engine's persistent named-analyzer map.

The process-global registry checks its custom entries before built-ins. Name collisions can therefore shadow persistent definitions and make behavior process-dependent. Durable applications must use distinct names and register catalog-owned definitions through `Engine::register_named_analyzer` or SQL `create_analyzer`.

The built-in default name is `standard`. `list_analyzers()` constructs a SQL-visible set from persistent custom names plus the four built-ins. `Engine::list_named_analyzers` and the CLI `\da` expose only the engine's persistent custom-name map.

## Field binding and phase resolution

Memory, Key/Value, and SQLite indexes share `AnalyzerBindings`. It retains immutable `Arc<CompiledAnalyzer>` handles for independent index and search sides. Infallible constructors defer default validation until first successful resolution and then retain that exact default. Explicit field assignment compiles before either side changes. Effective revisions are:

```mermaid
flowchart TD
    A[Resolve index revision] --> B{Field binding exists}
    B -->|Yes| C[Retained index revision]
    B -->|No| D[Retained table default]
    E[Resolve search revision] --> F{Field binding exists}
    F -->|Yes| G[Retained search revision]
    F -->|No| D
```

`AnalyzerPhase::Index` replaces only the index revision, `Search` replaces only the search revision, and `Both` installs one revision on both sides. A first phase-specific assignment retains the prior default on the unselected side. The phase parser accepts `index`, `search`, the `query` alias, and `both`; the engine persists `query` as normalized `search`.

The engine currently retains one durable assignment record per `(table, field)`. Calling `set_table_field_analyzer` replaces the previous record and phase. Providers now retain two independent compiled revisions during the current lifetime. The existing catalog still restores mutable names and only the last recorded phase. Durable exact descriptors, independent phase restoration, and catalog-epoch migration remain required work in the [Nori plan](../../plans/0006-nori-analyzer.md); runtime revision retention alone does not provide that persistence contract.

## Indexing path

```mermaid
sequenceDiagram
    participant SQL as SQL or typed write
    participant Engine
    participant Analyzer as Index analyzer
    participant Index as Inverted index
    participant Store as Document store
    SQL->>Engine: Insert or update document
    Engine->>Analyzer: Analyze every indexed text field
    Analyzer-->>Engine: Tokens or error
    Engine->>Index: Publish replacement postings and lengths
    Engine->>Store: Publish document
    Store-->>SQL: Commit success
```

Document writes project only registered FTS fields whose values are strings. The inverted-index write is a replacement operation so an update that removes indexed text cannot leave stale postings. Analyzer failure aborts before the document store publishes the new row. Transaction snapshots cover analyzer instances and catalog state so a later failure can restore the prior visible state.

`CREATE INDEX ... USING gin` calls `add_fts_field_with_analyzer` for every indexed column. It validates an optional analyzer name, installs it for both phases, registers the FTS field, and rebuilds the full index from existing documents. The catalog index row stores the analyzer option for reopen.

`set_table_field_analyzer` first requires a real `TEXT` column already registered in the physical FTS index. An `index` or `both` assignment installs the analyzer and calls `rebuild_fts_index`; a search-only assignment changes query analysis without touching postings. Persistence failure or rebuild failure restores the old index and search analyzers and rebuilds the prior posting state when required.

## Query path

```mermaid
sequenceDiagram
    participant Query
    participant Operator as Retrieval operator
    participant Analyzer as Search analyzer
    participant Index as Inverted index
    Query->>Operator: Field and query leaf
    Operator->>Analyzer: Analyze leaf text
    Analyzer-->>Operator: Zero or more terms
    loop Each analyzed term
        Operator->>Index: Open posting list or score cursor
    end
    Operator-->>Query: Union support and ranked scores
```

`TermOperator` retains `search_analyzer_revision(field)`, analyzes its term, returns empty support for zero tokens, and unions posting lists for multiple tokens. Search-time synonym expansion therefore broadens one leaf without requiring synonym postings for the source query token itself, provided each expansion already exists in the index vocabulary.

Engine text scoring, calibration, hybrid search, top-K execution, multi-field retrieval, retrieval planning, and the operator-tree driver retain the field search revision. Detached portal indexes install the exact compiled handles from their source index. Scoring code uses the analyzed term sequence for term-frequency and query-term accounting. Duplicate analyzed terms can remain semantically relevant and must not be deduplicated casually.

The SQL `uqa_highlight` scalar path is an exception: it extracts whitespace-separated query candidates and uses `standard_analyzer("english")` directly. It does not receive a table and field identity, so it cannot resolve a field analyzer. The typed highlighting API accepts an explicit analyzer.

## Catalog persistence and reopen

Persistent backends store named analyzer JSON separately from table-field assignment rows. Reopen validates and loads named definitions, validates each target field, resolves the assigned name, restores the normalized phase into the inverted index, and restores catalog indexes.

A GIN analyzer option and a later standalone field assignment are separate catalog owners. GIN restoration replays its analyzer option as part of the index definition. Callers should use one ownership path per field instead of relying on restoration order between two competing definitions.

Dropping a table or its last logical GIN reference removes field analyzer metadata. Dropping a named analyzer fails while any durable table-field assignment references its name. A DDL-owned analyzer must also remain resolvable for its GIN catalog index to reopen.

## Synonym resources

Inline synonyms are copied into the analyzer JSON. Named file-backed configurations still persist a path. Registration, a new compilation, uncompiled analysis, and legacy catalog restoration read that file; provider bindings retain the compiled snapshot with inline resolved maps. The parser supports blank lines, `#` comments, one-way `left => right` mappings, and comma-separated equivalent groups.

Installed provider revisions keep their output after the file is edited or removed. An explicit new binding resolves the current contents and publishes them only after its rebuild succeeds. A missing file still fails a new registration or compilation, a deferred default that has not resolved, and current legacy reopen validation. Persisting exact descriptor snapshots is a separate remaining catalog change; the current provider snapshot does not establish file-independent reopen.

## Transaction and cache behavior

Analyzer lifecycle functions are classified as mutating SQL even though they appear as table functions under `SELECT`. Implicit statement transactions and explicit transaction snapshots include named analyzers, table assignments, retained compiled handles, and affected postings. `rebuild_with_analyzer_revision` stages a replacement under the candidate handle and publishes its postings and selected binding sides together. Memory builds an independent replacement; Key/Value uses one batch; SQLite uses its transactional rebuild. Failed publication retains the previous binding. Memory bulk insertion also publishes only after the complete batch succeeds. An outer expression failure after `create_analyzer` has produced a row still rolls the creation back.

Named definition and assignment publication advances catalog or table epochs. Other sessions synchronize registry and table catalogs before use, and prepared or stored plans cannot treat cached analyzer-dependent execution state as authoritative after those epochs change.

## Required invariants

- A registered analyzer must validate before it becomes visible or persistent.
- A configured field must exist, be `TEXT`, and be a physical FTS field.
- Index-time analyzer changes must rebuild existing postings atomically.
- Search-time output must be compatible with the indexed vocabulary unless empty support is intended.
- Analyzer errors must remain errors; they cannot become empty tokens, empty results, or partial writes.
- Persistent definitions must resolve without process-local registration on every reopen.
- A named analyzer cannot be removed while a durable field assignment still references it.
- GIN DDL ownership and standalone field-assignment ownership must not compete for the same field.

## Verification evidence

| Contract | Test area |
| --- | --- |
| Component behavior and JSON round trips | `crates/uqa-analysis/tests/analysis` |
| Invalid regex, gram bounds, and built-in registry rules | `crates/uqa-analysis/tests/analysis/validation.rs` |
| Inline and reloadable file synonyms | `crates/uqa-analysis/tests/analysis/synonym_file.rs` |
| In-memory and SQLite phase behavior | `crates/uqa-storage/tests/inverted_index_analyzer.rs` |
| SQL create, list, bind, drop, rollback, and reopen | `crates/uqa-engine/tests/sql_analyzer_lifecycle.rs` |
| GIN backfill and analyzer option restoration | `crates/uqa-engine/tests/sql_fts_index_lifecycle.rs` |
| Statement rollback for mutating table functions | `crates/uqa-engine/tests/transaction_lifecycle.rs` |

## Source entry points

| Area | Path |
| --- | --- |
| Pipeline | [`crates/uqa-analysis/src/analyzer.rs`](../../../crates/uqa-analysis/src/analyzer.rs) |
| Character filters | [`crates/uqa-analysis/src/char_filter.rs`](../../../crates/uqa-analysis/src/char_filter.rs) |
| Tokenizers | [`crates/uqa-analysis/src/tokenizer.rs`](../../../crates/uqa-analysis/src/tokenizer.rs) |
| Token filters | [`crates/uqa-analysis/src/token_filter.rs`](../../../crates/uqa-analysis/src/token_filter.rs) |
| Engine catalog lifecycle | [`crates/uqa-engine/src/analyzers.rs`](../../../crates/uqa-engine/src/analyzers.rs) |
| Field registration and rebuild | [`crates/uqa-engine/src/table_storage/fts.rs`](../../../crates/uqa-engine/src/table_storage/fts.rs) |
| SQL argument rules | [`crates/uqa-sql/src/semantics/table_function_arguments.rs`](../../../crates/uqa-sql/src/semantics/table_function_arguments.rs) |
| SQL table-function execution | [`crates/uqa-execution/src/query/table_functions/analyzers.rs`](../../../crates/uqa-execution/src/query/table_functions/analyzers.rs) |
| Inverted-index contract | [`crates/uqa-storage/src/inverted_index/contract.rs`](../../../crates/uqa-storage/src/inverted_index/contract.rs) |

## Related documentation

- [Text analyzer reference](../reference/06-text-analyzers.md)
- [Analyzer SQL](../sql/05-analyzers.md)
- [Analyzer pipeline tutorial](../tutorials/03-analyzer-pipelines.md)
- [Search and ranking internals](05-search-and-ranking.md)
