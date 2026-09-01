# Search and Ranking

UQA Engine provides full-text, vector, tensor, and hybrid retrieval through SQL and typed engine APIs. Retrieval predicates produce ranked document support and expose the score as `_score` in SQL. In a query block with one unambiguous local-table source, `_meta.score` addresses that engine-owned score and `_meta.doc_id` addresses the storage document identity even when ordinary user columns are named `_score` or `_doc_id`.

## Full-text setup

A text column must belong to a GIN index before it can be searched:

```sql
CREATE TABLE articles (
    id INTEGER PRIMARY KEY,
    title TEXT NOT NULL,
    body TEXT NOT NULL,
    embedding VECTOR(4)
);

CREATE INDEX articles_text_gin
ON articles USING gin (title, body);
```

Search with BM25:

```sql
SELECT id, title, _score
FROM articles
WHERE text_match(body, 'rust database')
ORDER BY _score DESC, id ASC
LIMIT 20;
```

`fts_match` uses the richer full-text query grammar. The `@@` form maps to the same full-text matching path where the surrounding expression is valid.

## Analyzer selection

Every indexed text field has an analyzer pipeline for document indexing and query analysis. Without an explicit assignment, the field uses the built-in `standard` analyzer: standard tokenization, lowercase, ASCII folding, English stop-word removal, and Porter stemming.

For CJK-style text, assign the built-in `standard_cjk` analyzer. It extends the `standard` pipeline with 2-to-3-character n-grams and retains shorter tokens, which provides character-substring matching without claiming language-specific morphological segmentation:

```sql
SELECT * FROM set_table_analyzer(
    'articles',
    'body',
    'standard_cjk',
    'both'
);
```

The field must already be in a GIN index. The `both` assignment rebuilds existing postings and applies `standard_cjk` to later document and query analysis.

Custom analyzers are ordered JSON pipelines of character filters, one tokenizer, and token filters. They can normalize HTML-shaped input, split with standard, pattern, keyword, letter, whitespace, or n-gram tokenizers, and apply lowercase, stop words, stemming, ASCII folding, synonyms, n-grams, edge n-grams, or length filtering.

Use `create_analyzer` to register a pipeline and either name it in `CREATE INDEX ... WITH (analyzer = ...)` or assign it to an existing GIN field with `set_table_analyzer`. Analyzer phases are `index`, `search` or its `query` alias, and `both`. An index-time change rebuilds postings; a search-only change does not.

The complete JSON schema, exact component tags, SQL and Rust lifecycle, synonym-file format, phase fallback, persistence, and removal rules are in [Text analyzer pipelines](06-text-analyzers.md). The executable walkthrough is [Tutorial 3: Analyzer pipelines](../tutorials/03-analyzer-pipelines.md).

## Full-text query grammar

The search query parser supports:

- Bare terms: `rust database`
- Quoted phrases: `"embedded database"`
- Boolean operators: `AND`, `OR`, and `NOT`
- Parentheses: `(rust OR zig) AND database`
- Field scope: `title:rust`
- Field-scoped phrase: `title:"query engine"`
- Field-scoped vector literal for retrieval expressions

Operator precedence is `NOT`, then `AND`, then `OR`. Adjacent terms imply `AND`. Use parentheses whenever intent would otherwise depend on precedence.

## BM25

For a query term, UQA Engine uses the inverse document frequency

$$
\mathrm{IDF}(t) = \ln\left(\frac{N - n_t + 0.5}{n_t + 0.5} + 1\right),
$$

where $N$ is the number of indexed documents and $n_t$ is the document frequency of term $t$. Its term contribution is

$$
s_t = w_t - \frac{w_t}{1 + f_t I},
$$

with

$$
w_t = b_t \mathrm{IDF}(t), \qquad
I = \frac{1}{k_1\left((1-b) + b\frac{d}{\bar d}\right)}.
$$

Here $f_t$ is term frequency, $b_t$ is the query boost, $d$ is document length, and $\bar d$ is average document length. Defaults are $k_1 = 1.2$, $b = 0.75$, and $b_t = 1$.

`Engine::search` accepts `ScoringMode::BM25` or `ScoringMode::BayesianBM25`. `Engine::search_profiled` returns the selected physical algorithm and candidate, cursor, skip, and elapsed-time counters.

## Bayesian BM25

Bayesian BM25 calibrates the complete raw query score once:

$$
P(R = 1 \mid s) = \sigma(\alpha(s - \beta)), \qquad
\sigma(x) = \frac{1}{1 + e^{-x}}.
$$

The calibration parameters must be estimated or learned from representative data. A probability-shaped number is not proof of calibration. Evaluate held out labels with the calibration report, including Brier score, log loss, expected calibration error, and reliability bins.

SQL retrieval uses `bayesian_match`. The typed API uses `ScoringMode::BayesianBM25`.

## Vector and tensor columns

Declare a fixed dimension:

```sql
CREATE TABLE passages (
    id INTEGER PRIMARY KEY,
    body TEXT,
    embedding VECTOR(4),
    token_embeddings TENSOR(4)
);
```

Vectors must contain exactly the declared number of finite values. A tensor is a row-level collection of vectors with the same dimension. Tensor retrieval scores a row by its best matching element.

Run cosine KNN:

```sql
SELECT id, body, _score
FROM passages
WHERE knn_match(embedding, ARRAY[1.0, 0.0, 0.0, 0.0], 20)
ORDER BY _score DESC, id ASC
LIMIT 10;
```

The `k` argument controls the candidate pool. A later relational predicate is applied to that pool, so widen `k` when filtering could remove many candidates.

## Vector access paths

| Path | Creation | Result contract |
| --- | --- | --- |
| Brute force | No vector index | Exact cosine ranking |
| IVF | `USING ivf` | Approximate unless all relevant lists are probed |
| HNSW | `USING hnsw` | Approximate graph search |

IVF example:

```sql
CREATE INDEX passages_embedding_ivf
ON passages USING ivf (embedding)
WITH (lists = 64, probes = 8, train_threshold = 1000);
```

Accepted IVF option aliases include `lists` or `nlist`, `probes` or `nprobe`, and `train_threshold`, `train-threshold`, or `min_train`.

HNSW example:

```sql
CREATE INDEX passages_embedding_hnsw
ON passages USING hnsw (embedding)
WITH (m = 16, ef_construction = 200, ef_search = 64, seed = 42);
```

HNSW also accepts hyphenated aliases for `ef_construction`, `ef_search`, and `rebuild_threshold`. Only one physical vector index may own a vector column at a time. Drop the current index before selecting another physical access path.

Tune approximate indexes with measured recall and latency on production-shaped queries. Brute-force results are the exact reference for recall evaluation.

## Pair-producing retrieval joins

Retrieval operands can produce typed document pairs from SQL `FROM` through `text_similarity_join`, `vector_similarity_join`, `graph_join`, `hybrid_join`, and `cross_paradigm_join`. Each result preserves `left_doc_id` and `right_doc_id` in a `GeneralizedPostingList` and exposes `_score`; it can be joined to ordinary tables without collapsing pair identity into one document id.

These functions take a left relation before the left operand and a right relation before the right operand. Write each relation as `passages` or `app.passages`, without string quotes; each follows ordinary catalog and `search_path` resolution, and each operand is bound, optimized, and executed only against its adjacent relation. Repeat the same identifier for a self-join. When the function source has an alias and all cost-relevant arguments are bound, DPccp receives the independently estimated operand costs and pair cardinality and may reorder the source with table relations. See [Retrieval SQL](../sql/06-retrieval.md#operator-joins-as-sql-sources) for signatures and a cross-relation example.

## Hybrid retrieval

UQA Engine offers two distinct fusion contracts:

1. `fuse_bayesian_evidence`, its exact alias `fuse_log_odds`, automatic mixed-modality SQL, and `Engine::hybrid_search` combine calibrated, conditionally independent evidence exactly in signed log-odds space with one prior.
2. `pool_positive_evidence` and `Engine::robust_hybrid_search` implement the separately named robust positive-evidence ranking heuristic.

SQL automatically selects exact Bayesian evidence fusion when one `AND` conjunction contains supported text and vector retrieval leaves from the same relation. `text_match` is calibrated and stripped of its signal-local prior at that boundary, the KNN query pool is converted to prior-free vector evidence, the resolved corpus prior enters once, and ordinary conjuncts remain strict post-fusion filters. This policy makes conditional independence between the text and vector modalities the default hybrid contract. Relation-qualified signals from different sources are not combined, unqualified fields are inferred only in a single-source query block, and a joined query must qualify all inferred signals with the same relation alias.

Automatic text leaves are `text_match` and `bayesian_match`, and automatic vector leaves are `knn_match` and `calibrated_vector_match`. `bayesian_match_with_prior` remains outside inference because its document-level prior is not one removable corpus prior.

Exactness describes the prior and evidence algebra. The KNN evidence is still a query-pool likelihood-ratio estimate unless the application supplies a separately validated calibration contract.

Exact Bayesian evidence fusion is

$$
\mathrm{logit}(P) = \mathrm{logit}(\pi)
 + \sum_i \ell_i,
$$

where $\pi$ is the prior and each $\ell_i$ is prior-free evidence. Do not feed independent posterior probabilities into this equation without removing their priors. When exact signals report inferred corpus priors, those priors must agree numerically; otherwise the query must supply one explicit `base_rate` instead of inventing an averaging rule.

The positive-evidence pool is useful when calibrated conditional independence is not justified. It gates weak or negative contributions and, when adaptive weighting is enabled, can reduce the weight of a signal that does not separate the current candidate pool. Its `alpha` value controls confidence scaling and must be in its documented valid range.

Automatic SQL example:

```sql
SELECT id, title, _score
FROM articles
WHERE text_match(body, 'embedded retrieval')
  AND knn_match(embedding, ARRAY[0.8, 0.1, 0.1, 0.0], 100)
ORDER BY _score DESC, id ASC
LIMIT 20;
```

Explicit SQL example:

```sql
SELECT id, title, _score
FROM articles
WHERE pool_positive_evidence(
    bayesian_match(body, 'embedded retrieval'),
    knn_match(embedding, ARRAY[0.8, 0.1, 0.1, 0.0], 100)
)
ORDER BY _score DESC, id ASC
LIMIT 20;
```

`fuse_log_odds` is an exact alias for `fuse_bayesian_evidence`; it accepts the same optional `base_rate` named argument. Robust gating, confidence scaling, bounds, and weights belong only to `pool_positive_evidence`.

Any explicit fusion function overrides the automatic policy and is preserved without another fusion wrapper.

## Stable result handling

Always add a deterministic secondary key when repeatable ordering matters:

```sql
SELECT id, _score
FROM articles
WHERE text_match(body, 'embedded database')
ORDER BY _score DESC, id ASC;
```

Approximate indexes can change candidate membership when index parameters, build order, or data change. Score ties are otherwise unordered unless the SQL query supplies another ordering expression.

## Analyzer and index diagnostics

Named analyzers can be created, listed, assigned to a table field, and dropped through SQL table functions and engine APIs. `fts_index_stats` reports posting, term, document-length, indexed-document, analyzer, and total-field-length information. SQL `list_analyzers()` includes built-ins and custom catalog names, while the CLI command `\da` lists custom engine-catalog names. See [Analyzer SQL](../sql/05-analyzers.md) for exact signatures and result columns.

Use search profiles and index statistics to diagnose execution. Do not infer relevance quality only from query latency; evaluate labels and exact-ground-truth recall separately.

## Related guides

- [Full-text tutorial](../tutorials/02-full-text-search.md)
- [Vector and hybrid tutorial](../tutorials/04-vector-and-hybrid-search.md)
- [SQL retrieval reference](../sql/06-retrieval.md)
- [Text analyzer pipelines](06-text-analyzers.md)
- [Search internals](../internals/05-search-and-ranking.md)
- [Vector index design](../../design/vector-indexes.md)
