# Analyzer SQL

UQA-RS exposes analyzer catalog operations as row-producing SQL functions and binds analyzers to physical GIN fields. PostgreSQL `CREATE TEXT SEARCH CONFIGURATION`, dictionary, and parser DDL are not implemented; the supported contract is the JSON pipeline described here and in the [text analyzer reference](../reference/06-text-analyzers.md).

## Function summary

| Function | Arguments | Result | Mutates state |
| --- | --- | --- | --- |
| `create_analyzer(name, config_json)` | Two strings | One status row | Yes |
| `list_analyzers()` | None | One `analyzer_name` row per built-in or custom catalog analyzer | No |
| `set_table_analyzer(table, field, name [, phase])` | Three or four strings | One status row | Yes |
| `drop_analyzer(name)` | One string | One status row | Yes |
| `fts_index_stats([table])` | Zero or one table-name string | One diagnostics row per indexed field | No |

These functions are used in `FROM` like other table functions. `SELECT * FROM function(...)` is the direct lifecycle form.

## Analyzer JSON

The top-level schema is:

```json
{
  "char_filters": [],
  "tokenizer": {"type": "whitespace"},
  "token_filters": []
}
```

Stages run in this order: every `char_filters` entry, the one `tokenizer`, and every `token_filters` entry. Omitted arrays are empty and an omitted tokenizer defaults to case-sensitive `whitespace` tokenization. That empty default differs from the built-in analyzer named `whitespace`, which also lowercases.

### Character filter types

| Type | Required properties | Optional properties |
| --- | --- | --- |
| `h_t_m_l_strip` | None | None |
| `mapping` | `mapping` object from source string to replacement string | None |
| `pattern_replace` | `pattern` | `replacement`, default empty |

### Tokenizer types

| Type | Required properties |
| --- | --- |
| `whitespace` | None |
| `standard` | None |
| `letter` | None |
| `n_gram` | `min_gram`, `max_gram` |
| `pattern` | `pattern` |
| `keyword` | None |

### Token filter types

| Type | Required properties | Optional properties |
| --- | --- | --- |
| `lowercase` | None | None |
| `stop` | None | `language`, default `english`; `custom_words`, default empty |
| `porter_stem` | None | None |
| `a_s_c_i_i_folding` | None | None |
| `synonym` | `synonyms` or `synonyms_path` | The unused source can be omitted |
| `ngram` | `min_gram`, `max_gram` | `keep_short`, default false |
| `edge_ngram` | `min_gram`, `max_gram` | None |
| `length` | None | `min_length`, default 0; `max_length`, default 0 for unlimited |

`h_t_m_l_strip` and `a_s_c_i_i_folding` are the exact current JSON tags. Gram bounds require a positive minimum and a maximum not smaller than the minimum. Pattern fields use Rust regular-expression syntax.

## CREATE ANALYZER function

`create_analyzer` parses and validates the complete configuration before publishing it:

```sql
SELECT * FROM create_analyzer(
    'html_vehicle',
    $analyzer$
{
  "char_filters": [{"type": "h_t_m_l_strip"}],
  "tokenizer": {"type": "standard"},
  "token_filters": [
    {"type": "lowercase"},
    {"type": "a_s_c_i_i_folding"},
    {"type": "stop", "language": "english", "custom_words": ["manual"]},
    {"type": "synonym", "synonyms": {"car": ["automobile"], "automobile": ["car"]}}
  ]
}
$analyzer$
);
```

The second argument is a SQL string containing JSON. Tagged dollar quoting keeps the JSON readable without escaping double quotes. An invalid JSON document, unknown component tag, invalid regular expression, invalid gram range, or unavailable synonym file rejects the statement.

Registering the same custom name replaces its stored JSON definition, but already materialized postings are not rebuilt merely because the named definition changed. Reapply an index-owned definition by recreating its GIN index, or reapply a field-owned definition with `set_table_analyzer` and an `index` or `both` phase.

## LIST ANALYZERS function

```sql
SELECT analyzer_name
FROM list_analyzers()
ORDER BY analyzer_name;
```

The result includes `keyword`, `standard`, `standard_cjk`, `whitespace`, and custom analyzer names stored in the engine catalog. The function accepts no arguments.

## Bind through CREATE INDEX

```sql
CREATE INDEX articles_body_gin
ON articles USING gin (body)
WITH (analyzer = 'html_vehicle');
```

The analyzer must already resolve when the index is created. It is applied to both index and search phases, existing rows are backfilled, and the name is persisted in the index definition. To change an analyzer owned by this DDL, drop and recreate the GIN index.

## SET TABLE ANALYZER function

Create a GIN index without an analyzer option when assignments will be managed separately:

```sql
CREATE INDEX articles_body_gin
ON articles USING gin (body);

SELECT * FROM set_table_analyzer(
    'articles',
    'body',
    'html_vehicle',
    'both'
);
```

The target table and column must exist, the column must be `TEXT`, and the field must already be part of a physical GIN index. The optional phase is case-insensitive and has this contract:

| Phase | Contract |
| --- | --- |
| `index` | Install for future document analysis and rebuild all current postings |
| `search` or `query` | Install for query analysis without rebuilding postings |
| `both` | Install for both sides and rebuild all current postings |

The default phase is `both`. One assignment row is retained per table field, so another call replaces its recorded analyzer and phase. A phase-specific call updates only that in-memory side and leaves the other side unchanged, but only the last assignment is restored after reopen. Do not layer separate index and search assignments; prefer one `both` assignment or verify the complete asymmetric lifecycle. Do not combine a GIN `analyzer` option with a separate field assignment for the same column; select one catalog owner.

## Search behavior

The field's search analyzer is used by full-text retrieval leaves:

```sql
SELECT id, title, _score
FROM articles
WHERE text_match(body, 'car')
ORDER BY _score DESC, id ASC;
```

If no search-phase analyzer is installed, execution falls back to the field's index analyzer and then to the table's default analyzer. Multiple tokens emitted for one query leaf, including synonym expansions, are unioned across posting lists. BM25 scoring uses the analyzed term sequence.

## FTS INDEX STATS function

Inspect every GIN field or filter by table:

```sql
SELECT table_name, field, analyzer, posting_count,
       doc_length_count, indexed_doc_count,
       term_count, total_field_length
FROM fts_index_stats('articles')
ORDER BY field;
```

| Column | Meaning |
| --- | --- |
| `table_name` | Resolved table name |
| `field` | Indexed text column |
| `analyzer` | Recorded field analyzer name or `standard` when no assignment exists |
| `posting_count` | Number of document postings in the field |
| `doc_length_count` | Number of stored field-length entries |
| `indexed_doc_count` | Indexed-document count, currently the field-length count |
| `term_count` | Distinct indexed terms |
| `total_field_length` | Sum of analyzed token counts used for length normalization |

The zero-argument form returns all indexed fields. More than one argument or a non-string table argument is rejected.

## DROP ANALYZER function

```sql
SELECT * FROM drop_analyzer('html_vehicle');
```

Dropping a missing analyzer is an error. Dropping a custom analyzer still assigned to a field is also an error. Replace the field assignment with a built-in analyzer before dropping an assignment-owned analyzer:

```sql
SELECT * FROM set_table_analyzer(
    'articles',
    'body',
    'standard',
    'both'
);

SELECT * FROM drop_analyzer('html_vehicle');
```

For a GIN-index-owned analyzer, recreate the GIN index without that name before dropping the definition. Built-in analyzers are not custom catalog entries and are not drop targets.

## Transactions and persistence

Analyzer creation, assignment, and deletion are mutating SQL operations. Each standalone statement has an implicit transaction, and the operations can participate in explicit transactions. A failing outer projection or later statement rollback does not leave a partially published analyzer, assignment, or rebuilt posting set.

Persistent engines store custom analyzer JSON and table-field assignments in the catalog and restore them on reopen. A file-backed synonym configuration stores only its path. Reopen and later analysis require that path to remain readable.

## Limits and deliberate differences

- UQA-RS does not implement PostgreSQL text-search parser, dictionary, configuration, or template DDL.
- SQL has no analyzer token-preview function; construct `uqa_analysis::Analyzer` and call `analyze` in Rust to inspect a pipeline directly.
- SQL does not return the raw JSON definition for every named analyzer. Rust `Engine::get_table_analyzer` returns the normalized serialized configuration for an assigned phase.
- SQL `uqa_highlight` uses the built-in English `standard` analyzer rather than inheriting the field assignment.
- One durable assignment is recorded per field, not one independently managed assignment for each phase.

## Related documentation

- [Text analyzer pipelines](../reference/06-text-analyzers.md)
- [Analyzer pipeline tutorial](../tutorials/03-analyzer-pipelines.md)
- [Retrieval SQL](06-retrieval.md)
- [Analyzer internals](../internals/04-analyzer-pipeline.md)
