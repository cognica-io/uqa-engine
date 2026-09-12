# Retrieval SQL

Retrieval functions create document support, scores, or transformations in the unified plan. They are not interchangeable with ordinary Boolean scalar functions even though many appear in `WHERE`.

## Core retrieval predicates

| Function | Purpose |
| --- | --- |
| `text_match(field, query)` | BM25 full-text retrieval |
| `fts_match(field, query)` | Full-text retrieval with explicit query grammar |
| `bayesian_match(field, query)` | Bayesian-calibrated BM25 retrieval |
| `bayesian_match_with_prior(field, query, prior)` | Bayesian BM25 with an explicit prior contract |
| `knn_match(field, vector, k)` | Cosine KNN candidate retrieval |
| `calibrated_vector_match(...)` | Vector retrieval with pool-based calibration |
| `multi_field_match(...)` | Retrieval across indexed text fields |
| `staged_retrieval(...)` | Multi-stage candidate and reranking plan |

A GIN index is required for every searched text field. Vector inputs must match the declared `VECTOR(n)` or `TENSOR(n)` dimension and contain finite values.

## Row metadata

`_score` and `_doc_id` remain available for compatibility. A query block with one unambiguous local-table source can also address the same engine-owned values as `_meta.score` (`DOUBLE PRECISION`) and `_meta.doc_id` (`BIGINT`). The `_meta` form is useful when the table has ordinary user columns named `_score` or `_doc_id`: those bare or table-qualified names continue to select the user values, while `_meta.score` and `_meta.doc_id` select the retrieval score and storage document identity independently. An unranked scan exposes `0.0` as its metadata score.

```sql
SELECT id,
       _score AS stored_score,
       _meta.score AS retrieval_score,
       _doc_id AS stored_document_name,
       _meta.doc_id AS storage_document_id
FROM documents
WHERE text_match(body, 'database')
ORDER BY _meta.score DESC, id ASC;
```

The metadata namespace is binding-only: it adds nothing to `*`, and `_meta.*` is not a relation wildcard. When a query block contains more than one local-table metadata source, qualify and project the desired metadata inside a single-source subquery before joining. A real relation alias named `_meta` retains ordinary SQL name resolution and disables the virtual namespace for that query block.

## Analyzer resolution

Document text and query leaves are transformed by a field-specific analyzer pipeline. Query execution resolves an explicit search analyzer, then falls back to the field's index analyzer, then to the built-in `standard` analyzer. Multiple tokens emitted for one leaf, including synonym expansions, are unioned across posting lists and feed scoring term accounting.

Register custom JSON with `create_analyzer`, bind it through a GIN `analyzer` option or `set_table_analyzer`, and inspect it with `list_analyzers()` and `fts_index_stats`. Use an `index` or `both` phase when document analysis changes; this rebuilds current postings. Use `search` only when every emitted term is compatible with the existing index vocabulary. See [Analyzer SQL](05-analyzers.md) for the exact contract.

## Full-text query syntax

The query string accepts terms, quoted phrases, `AND`, `OR`, `NOT`, parentheses, field scoping, and field-scoped vector literals. Precedence is `NOT`, then `AND`, then `OR`; adjacency implies `AND`.

A quoted phrase is analyzed as one complete input with the field's retained search analyzer. It matches connected token paths in order, with exact adjacency and any internal gaps left by removed tokens. Synonym and compound alternatives preserve their positions and lengths; matching follows each query and document edge's own length. Leading and trailing removed tokens do not anchor the phrase to either end of the field, and a phrase with no remaining tokens matches no documents. All-field search requires the complete phrase to match within one field. Position filtering happens before score ordering and `LIMIT`; ordinary unquoted leaves continue to union their analyzed terms.

The physical phrase node remains intact through optimization. Phrase BM25 scores preserve emitted query-term multiplicity, including repeated terms; complete Boolean query calibration uses those emitted phrase units along with the existing unquoted leaf units.

```sql
SELECT id, _score
FROM documents
WHERE fts_match(body, '(database OR retrieval) AND NOT legacy')
ORDER BY _score DESC, id ASC
LIMIT 20;
```

```sql
SELECT id, _score
FROM documents
WHERE fts_match(body, '"information retrieval"')
ORDER BY _score DESC, id ASC
LIMIT 20;
```

## BM25 and Bayesian BM25

`text_match` computes BM25 with document frequency, term frequency, document length, and average field length. `bayesian_match` applies one sigmoid calibration to the complete raw query score rather than independently calibrating and summing each term.

```sql
SELECT id, title, _score
FROM documents
WHERE bayesian_match(body, 'embedded query engine')
ORDER BY _score DESC, id ASC;
```

Calibration parameters must be learned or estimated for the table and field. Validate probability quality with held out labels; the engine calibration report includes expected calibration error, Brier score, log loss, and reliability bins.

## KNN

```sql
SELECT id, title, _score
FROM documents
WHERE knn_match(embedding, $1, 100)
ORDER BY _score DESC, id ASC
LIMIT 20;
```

The `k` argument defines the support delivered by the vector leaf. A later relational predicate filters that support; it does not automatically increase the vector pool. Widen `k` when downstream filters or fusion need more candidates.

With no vector index, KNN is an exact brute-force cosine scan. IVF and HNSW are approximate physical paths. A `TENSOR(n)` row uses its best element score.

## Automatic hybrid fusion

A direct `AND` conjunction that contains at least one supported text signal and at least one supported vector signal from the same relation is a hybrid retrieval request. The planner replaces those retrieval leaves with one `fuse_bayesian_evidence` node, calibrates `text_match` as Bayesian BM25 at the fusion boundary, converts the KNN query pool to prior-free vector evidence, and leaves every ordinary relational conjunct as a strict filter over the fused support.

```sql
SELECT id, title, _score
FROM documents
WHERE text_match(body, 'rust database')
  AND knn_match(embedding, $1, 200)
  AND status = 'published'
ORDER BY _score DESC, id ASC
LIMIT 20;
```

The automatic policy treats text and vector modalities as conditionally independent evidence. It removes each signal-local prior, adds their signed evidence in log-odds space, and applies the resolved corpus relevance prior exactly once. Signals qualified by different relations are never fused, unqualified fields are eligible only in a single-source query block, a joined query must qualify all inferred signals with the same relation alias, and two text leaves or two vector leaves retain their ordinary Boolean behavior.

The log-odds combination is exact under that contract; the KNN likelihood-ratio evidence remains an unsupervised estimate fitted from the selected query pool rather than a held-out probability calibration guarantee.

The supported automatic text leaves are `text_match` and `bayesian_match`; the supported vector leaves are `knn_match` and `calibrated_vector_match`. A prior-bearing leaf such as `bayesian_match_with_prior` is not inferred as independent evidence because its document-level prior cannot be removed as one corpus constant.

Write a fusion function explicitly to override the default policy, select robust positive-evidence pooling, use learned or attention fusion, or provide fusion options. An explicit fusion node is preserved and is never wrapped in another automatic fusion node.

## Fusion functions

| Function | Contract |
| --- | --- |
| `fuse_bayesian_evidence(...)` | Exact prior plus prior-free evidence accumulation in log-odds space; conflicting inferred priors require an explicit `base_rate` |
| `pool_positive_evidence(...)` | Robust positive-evidence heuristic with gating |
| `fuse_log_odds(...)` | Exact alias for `fuse_bayesian_evidence(...)` |
| `attention`, `fuse_attention`, `fuse_multihead` | Attention-based signal fusion |
| `learned_fusion`, `fuse_learned` | Registered learned fusion model |

Exact Bayesian evidence fusion is

$$
P = \sigma\left(\mathrm{logit}(\pi) + \sum_i \ell_i\right),
$$

where $\pi$ is the prior and each $\ell_i$ is prior-free evidence. Counting a posterior's prior again produces an incorrect result.

Positive-evidence pooling is deliberately a robust ranking heuristic rather than an exact conditional-independence statement:

```sql
SELECT id, title, _score
FROM documents
WHERE pool_positive_evidence(
    bayesian_match(body, 'rust database'),
    knn_match(embedding, $1, 200)
)
ORDER BY _score DESC, id ASC
LIMIT 20;
```

## Score transforms and thresholds

| Function | Purpose |
| --- | --- |
| `score_bm25` | Explicit BM25 scoring operator |
| `score_bayesian_bm25` | Explicit Bayesian BM25 scoring operator |
| `sparse_threshold` | Remove support below a score threshold |
| `convolve`, `pool`, `flatten`, `dense`, `softmax` | Model and tensor transformations |
| `layer`, `model` | Address registered model components |

These functions are lowered into operator trees when used in supported retrieval shapes. An unsupported placement fails rather than being treated as an arbitrary scalar call.

## Deep and learned retrieval

`deep_predict`, `deep_learn`, and the layer or model functions connect registered model state to query execution. Engine execution currently uses the CPU implementation; the direct `uqa-ml/mlx` crate feature is an experimental probe and is neither selected by SQL nor shipped as a supported backend. Learned state has explicit persistence and catalog ownership, while runtime callback and future accelerator state remain process-local.

Use held out evaluation and version every feature schema, model identity, and calibration set together. A model score has no stable interpretation if feature ordering or normalization changes independently.

## Highlighting and facets

`uqa_highlight(text, query [, start_tag, end_tag, max_fragments, fragment_size, analyzer])` returns highlighted source text. All arguments are expressions; the optional seventh argument names a built-in or registered analyzer.

| Argument | Meaning and default |
| --- | --- |
| `text` | Original source text; a NULL source returns NULL |
| `query` | Query text; NULL returns the original source before evaluating later arguments |
| `start_tag`, `end_tag` | Text inserted around matches; omitted or NULL values use `<b>` and `</b>` |
| `max_fragments` | Non-negative integer; omitted, NULL, or `0` returns the complete source |
| `fragment_size` | Positive target size in Unicode characters, default `150`; a selected match stays whole even when longer than the target |
| `analyzer` | Analyzer name; omitted or NULL preserves the built-in English word-scanning behavior |

With an explicit analyzer name, one immutable resolved revision analyzes the complete source and complete query text. Matching terms retain their original source spans through character replacement, HTML stripping, morphology, reading conversion, and token filters. Overlapping spans merge when markers are rendered; adjacent spans remain separate. Raw UTF-16 terms match by exact identity while markers cover complete original UTF-8 scalars. Query text is passed to the analyzer as plain text, so Boolean words are not removed by a separate query parser. Without a name, query candidates are split on whitespace, `AND`, `OR`, and `NOT` are removed, and the existing English analyzer checks individual source words.

The result is one TEXT value. Highlighting reads the named revision visible to the session without changing catalog or index state, and it does not infer a table-field analyzer from a bare text value. It needs no GIN index. Later calls observe committed name replacements and transaction/savepoint restoration; each call retains the same revision for its source and query.

Invalid argument counts outside `2..=7`, non-text query/tag/analyzer arguments, negative fragment counts, and non-positive fragment sizes fail. Empty or unknown analyzer names, unavailable resources, and analysis failures also fail without falling back to another analyzer. NULL source/query short circuits take precedence over validation or evaluation of later arguments.

```sql execute
SELECT uqa_highlight('c++', 'c++', NULL, NULL, NULL, NULL, 'keyword') AS snippet;
```

This returns `<b>c++</b>`. The typed Rust `uqa_analysis::highlight` API accepts an explicit analyzer and uses the same original-source spans; `highlight_compiled` accepts an already retained compiled revision. `uqa_facets` computes facet output over retrieval support. Apply these helpers after establishing the intended candidate set and preserve the original field separately when highlighted output is rendered as HTML.

Never treat highlighted text as trusted HTML solely because the engine inserted markers. Escape source content according to the rendering context.

## Retrieval plus graph

`traverse_match`, `temporal_traverse`, and graph scoring functions can create or modify support before fusion. `rpq`, `graph_traverse`, `graph_neighbors`, and centrality functions are detailed in [Graph SQL and Cypher](07-graph.md).

## Operator joins as SQL sources

Five tuple-producing join operators are exposed in `FROM`. The first and third arguments are table identifiers, not string values: the expression immediately after each identifier is bound, optimized, costed, and executed against that relation. Unqualified and schema-qualified identifiers use the same catalog and `search_path` resolution as ordinary table references. The result has columns `left_doc_id BIGINT`, `right_doc_id BIGINT`, and `_score DOUBLE PRECISION`; a table-function column alias list may rename them. Repeat the same relation identifier in both positions for a self-join.

### Shared contract

| Contract part | Definition |
| --- | --- |
| Syntax | `join_function(left_relation, left_expression, right_relation, right_expression [, options...])` as a `FROM` source |
| Arguments | Each relation is an SQL identifier and cannot be quoted as a string or replaced by a value parameter; each operand is bound only to its adjacent relation |
| Result | A relation with `left_doc_id BIGINT`, `right_doc_id BIGINT`, and `_score DOUBLE PRECISION`; pair identity is preserved |
| Effects | Read-only; the source participates in ordinary relational planning and cost estimation |
| Errors | Wrong arity, an unresolvable relation or field, invalid threshold ranges, incompatible operand shapes, malformed or non-finite vectors, and unsupported scalar placement are rejected |
| Composition | Alias the result and join it normally, or wrap it in a subquery or CTE; do not pass one tuple-producing join call directly as another join function's scalar operand |

| Function | Join contract |
| --- | --- |
| `text_similarity_join(left_relation, left, right_relation, right, threshold)` | Jaccard similarity over the text fields named by the operands; `threshold` is in `[0, 1]` |
| `vector_similarity_join(left_relation, left, right_relation, right, threshold)` | Cosine similarity over the vector fields named by the operands; `threshold` is in `[-1, 1]` |
| `graph_join(left_relation, left, right_relation, right, label, graph)` | Directed graph-edge join from left identities to right identities; `label` is a string or `NULL` |
| `hybrid_join(left_relation, left, right_relation, right)` | Structured equijoin followed by cosine similarity of at least `0.5`; the structured and vector field names may differ between relations |
| `cross_paradigm_join(left_relation, left, right_relation, right)` | Graph-vertex-property to document-field equijoin; the left operand identifies one graph and the operands identify the property fields |

The following query compares live passages with an archive whose vector column has a different name, then joins both identities back to ordinary SQL rows. Because `pairs` is an aliased, fully bound source, its independently estimated operand cardinalities and access costs participate in DPccp with the ordinary relations.

```sql
SELECT pairs.left_doc_id, pairs.right_doc_id, pairs._score, p.title, a.title AS archived_title
FROM vector_similarity_join(
    passages,
    knn_match(embedding, ARRAY[1.0, 0.0, 0.0, 0.0], 100),
    archived_passages,
    knn_match(archived_embedding, ARRAY[0.8, 0.1, 0.1, 0.0], 100),
    0.85
) AS pairs
JOIN passages AS p ON p.id = pairs.left_doc_id
JOIN archived_passages AS a ON a.id = pairs.right_doc_id
ORDER BY pairs._score DESC, pairs.left_doc_id, pairs.right_doc_id;
```

An operator-join result is a normal SQL source, so it can participate in multiway joins or be wrapped in a subquery or CTE. Tuple-producing join functions are not scalar operands, however: nest their result relationally instead of passing one `*_join(...)` call directly as an operand to another.

For example, relational nesting can filter one operator join before composing it with another table:

```sql
WITH close_pairs AS (
    SELECT left_doc_id, right_doc_id, _score
    FROM vector_similarity_join(
        passages,
        knn_match(embedding, ARRAY[1.0, 0.0, 0.0, 0.0], 100),
        archived_passages,
        knn_match(archived_embedding, ARRAY[0.8, 0.1, 0.1, 0.0], 100),
        0.85
    )
    WHERE _score >= 0.90
)
SELECT close_pairs.left_doc_id, close_pairs.right_doc_id, p.title, a.title AS archived_title
FROM close_pairs
JOIN passages AS p ON p.id = close_pairs.left_doc_id
JOIN archived_passages AS a ON a.id = close_pairs.right_doc_id
ORDER BY close_pairs.left_doc_id, close_pairs.right_doc_id;
```

The operator result preserves pair identity in `GeneralizedPostingList`; it does not collapse a pair to one synthetic document id. Text and vector joins place their similarity in `_score`, while graph and hybrid operators preserve their documented merged-score contract. Arguments that remain unbound at planning time retain SQL source order until they can be costed without guessing.

## Physical text top-K

The planner can select exhaustive scoring, WAND, or Block-Max WAND. WAND paths use score bounds to skip candidates while preserving exact top-K semantics. If a bound is stale or cannot prove safe skipping, execution falls back to safe scoring behavior.

Persistent postings are clustered by `doc_id / 65536`, delta-encode document IDs, separate score payloads from positions, and decode score blocks of at most 128 postings. These details affect physical performance, not SQL result semantics.

## Ordering and pagination

Always specify a deterministic tie key:

```sql
SELECT id, _score
FROM documents
WHERE text_match(body, 'database')
ORDER BY _score DESC, id ASC;
```

Offset pagination over a changing ranked corpus is unstable and can become expensive. For an API that requires stable continuation, capture a snapshot and use a seek boundary based on the full ordering tuple when the query surface permits it.

## Diagnostics

Use `fts_index_stats(table)` for index state, `Engine::search_profiled` for text physical counters, `EXPLAIN` for SQL planning, and exact brute-force vector results for approximate recall. Evaluate latency, relevance, calibration, and recall as separate metrics.
