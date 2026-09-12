# Text Analyzer Pipelines

An analyzer converts source text into the token stream used by a full-text index or a search expression. UQA Engine analyzers are ordered, named, field-bound pipelines; analyzer selection is therefore part of the search schema rather than a presentation-only setting.

## Pipeline model

Every analyzer executes the same three stages:

```mermaid
flowchart LR
    A[Input text] --> B[Character filters in array order]
    B --> C[One tokenizer]
    C --> D[Token filters in array order]
    D --> E[Token stream]
    E --> F[Index postings]
    E --> G[Query terms]
```

Character filters transform the complete string before tokenization. The tokenizer creates tokens. Token filters then transform, remove, or expand those tokens in the configured order. Array order is semantic: moving lowercase, stemming, stop-word removal, synonyms, or n-grams changes the resulting vocabulary.

The indexing phase analyzes document field values before writing postings. The search phase analyzes query leaves before posting lookup and scoring. A field can use an analyzer for `index`, `search`, or `both`; `both` is the default and is the safest choice when one vocabulary must be shared by documents and queries.

## Built-in analyzers

| Name | Pipeline | Typical use |
| --- | --- | --- |
| `standard` | `standard` tokenizer, lowercase, ASCII folding, English stop words, Porter stemming | Default English-oriented prose |
| `whitespace` | `whitespace` tokenizer, lowercase | Pre-segmented text whose punctuation must remain inside tokens |
| `standard_cjk` | `standard` pipeline followed by character n-grams from 2 through 3 with short-token retention | CJK-style text and substring-oriented matching |
| `keyword` | `keyword` tokenizer with no filters | Treat the complete non-empty field as one exact token |

`standard` is the default analyzer when a GIN field has no explicit assignment. `standard_cjk` is a character n-gram extension, not a language-specific morphological segmenter. SQL `list_analyzers()` includes the four built-ins and custom catalog analyzers.

Bind the CJK-oriented built-in directly to an indexed field; it does not need a custom JSON definition:

```sql
SELECT * FROM set_table_analyzer(
    'articles',
    'body',
    'standard_cjk',
    'both'
);
```

## JSON configuration shape

A custom analyzer is stored as JSON with one tokenizer and optional ordered filter arrays:

```json
{
  "char_filters": [
    {"type": "html_strip"}
  ],
  "tokenizer": {"type": "standard"},
  "token_filters": [
    {"type": "lowercase"},
    {"type": "ascii_folding"},
    {"type": "stop", "language": "english", "custom_words": ["manual"]},
    {"type": "synonym", "synonyms": {"car": ["automobile"], "automobile": ["car"]}},
    {"type": "length", "min_length": 2, "max_length": 24}
  ]
}
```

`html_strip` and `ascii_folding` are the serialized names and must be written exactly. The tokenizer defaults to `whitespace` when omitted; both filter arrays default to empty. This empty default is case-sensitive and differs from the built-in analyzer named `whitespace`, which adds lowercase filtering. The tokenizer and parameterless token filters also accept string shorthand, such as `{"tokenizer":"keyword","token_filters":["lowercase"]}`, but canonical object form is clearer when configurations are reviewed or generated.

## Character filters

| JSON type | Configuration | Behavior |
| --- | --- | --- |
| `html_strip` | None | Replaces tag-shaped text with spaces and decodes the built-in `amp`, `lt`, `gt`, `quot`, `#39`, `apos`, and `nbsp` entities |
| `mapping` | `mapping` object | Applies string replacements longest-key-first |
| `pattern_replace` | `pattern`, optional `replacement` | Replaces every Rust regular-expression match; replacement defaults to an empty string |

The HTML filter is a search normalization filter, not a validating HTML parser or sanitizer. Sanitize untrusted HTML at the application boundary according to its rendering context.

## Tokenizers

| JSON type | Configuration | Behavior |
| --- | --- | --- |
| `whitespace` | None | Splits with Unicode whitespace boundaries |
| `standard` | None | Extracts Unicode regular-expression word runs, including letters, digits, and underscores |
| `letter` | None | Extracts ASCII letter runs with `[a-zA-Z]+` |
| `n_gram` | `min_gram`, `max_gram` | Emits every character n-gram in the inclusive range for each whitespace-delimited word |
| `pattern` | `pattern` | Splits on a Rust regular expression and discards empty pieces |
| `keyword` | None | Emits the complete non-empty input as one token |

N-gram bounds require `min_gram > 0` and `max_gram >= min_gram`. A pattern tokenizer must contain a valid Rust regular expression.

## Token filters

| JSON type | Configuration | Behavior |
| --- | --- | --- |
| `lowercase` | None | Applies Unicode lowercase conversion |
| `stop` | Optional `language`, optional `custom_words` | Removes built-in English stop words plus exact custom words; the default language is `english` |
| `porter_stem` | None | Applies the built-in Porter stemmer |
| `ascii_folding` | None | Uses Unicode decomposition to fold characters with ASCII equivalents and preserves characters without one |
| `synonym` | Inline `synonyms` or `synonyms_path` | Retains each source token and appends its configured expansions |
| `ngram` | `min_gram`, `max_gram`, optional `keep_short` | Emits every character n-gram; a shorter token is retained only when `keep_short` is true |
| `edge_ngram` | `min_gram`, `max_gram` | Emits token prefixes in the inclusive range |
| `length` | Optional `min_length`, optional `max_length` | Retains tokens inside the character-count range; `max_length = 0` means no upper bound |

Only `english` has a built-in stop-word list. Other language strings contribute no built-in words, although `custom_words` still apply. Put `lowercase` before stop words or lowercase synonym keys when matching should be case-insensitive. Put synonyms before or after stemming deliberately because synonym keys and expansions are interpreted at that exact point in the stream.

## Create and bind an analyzer with SQL

Register the JSON under a catalog name:

```sql
SELECT * FROM create_analyzer(
    'html_vehicle',
    $analyzer$
{
  "char_filters": [{"type": "html_strip"}],
  "tokenizer": {"type": "standard"},
  "token_filters": [
    {"type": "lowercase"},
    {"type": "synonym", "synonyms": {"car": ["automobile"], "automobile": ["car"]}}
  ]
}
$analyzer$
);
```

List SQL catalog and built-in analyzer names:

```sql
SELECT analyzer_name
FROM list_analyzers()
ORDER BY analyzer_name;
```

There are two binding paths. A GIN index can own the analyzer as part of its DDL:

```sql
CREATE INDEX articles_body_gin
ON articles USING gin (body)
WITH (analyzer = 'html_vehicle');
```

The DDL path applies the analyzer to both indexing and search, validates the name, and backfills existing rows. Because the analyzer name remains part of the durable index definition, change this choice by dropping and recreating the GIN index.

Alternatively, create the GIN index without an analyzer option and manage the field assignment separately:

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

`set_table_analyzer` requires an existing `TEXT` column already present in a physical GIN index. One durable assignment is recorded per table field, so a later call replaces the previous catalog record. A phase-specific call updates only its selected in-memory side and does not clear the other side; do not layer separate index and search calls because only the last assignment is restored after reopen. Prefer one `both` assignment unless an asymmetric pipeline has a tested lifecycle. Use either DDL ownership or field-assignment ownership for a field; do not depend on a mixture of both catalog paths.

## Analyzer phases

| Phase | Document writes | Existing postings | Query analysis |
| --- | --- | --- | --- |
| `index` | Uses the assigned analyzer | Rebuilt immediately when assigned | Uses an explicit search analyzer when present, otherwise falls back to the index analyzer |
| `search` or `query` | Keeps the current index analyzer | Not rebuilt | Uses the assigned analyzer |
| `both` | Uses the assigned analyzer | Rebuilt immediately when assigned | Uses the assigned analyzer |

Asymmetric phases are useful only when the resulting search terms remain compatible with the indexed vocabulary. Search-time synonym expansion is a common intentional asymmetry because one query token can be unioned across several existing posting terms. Index-only stemming or n-grams require a compatible search pipeline or queries can produce terms that do not exist in the index.

## Search and inspect the result

All text retrieval paths resolve the field's search analyzer, including typed text search, `text_match`, `fts_match`, Bayesian BM25, and multi-field retrieval:

```sql
SELECT id, title, _score
FROM articles
WHERE text_match(body, 'car')
ORDER BY _score DESC, id ASC;
```

Inspect physical index counts and the recorded analyzer name:

```sql
SELECT table_name, field, analyzer, posting_count,
       indexed_doc_count, term_count, total_field_length
FROM fts_index_stats('articles')
ORDER BY field;
```

The CLI command `\da` and `Engine::list_named_analyzers` list custom engine-catalog analyzer names. SQL `list_analyzers()` additionally includes the four built-ins.

## Change or remove a custom analyzer

An assigned analyzer cannot be dropped. For a field-assignment-owned analyzer, assign a replacement first and then drop the custom definition:

```sql
SELECT * FROM set_table_analyzer(
    'articles',
    'body',
    'standard',
    'both'
);

SELECT * FROM drop_analyzer('html_vehicle');
```

Changing the `index` or `both` phase rebuilds the complete full-text index before the assignment is published. A failure restores the prior analyzer and postings. If the analyzer was named in `CREATE INDEX ... WITH (analyzer = ...)`, drop and recreate that index with the replacement before dropping the custom analyzer.

## Rust APIs

Durable engine configuration uses JSON through `Engine`:

```rust
use uqa_engine::Engine;

let engine = Engine::new();
let config = r#"{
  "tokenizer":{"type":"standard"},
  "token_filters":[{"type":"lowercase"},{"type":"porter_stem"}],
  "char_filters":[{"type":"html_strip"}]
}"#;

engine.register_named_analyzer("html_english", config)?;
engine.sql("CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT)", &[])?;
engine.sql("CREATE INDEX docs_body_gin ON docs USING gin (body)", &[])?;
engine.set_table_field_analyzer("docs", "body", "html_english", "both")?;

let assignment = engine.table_field_analyzer("docs", "body")?;
assert_eq!(assignment, Some(("html_english".into(), "both".into())));
# Ok::<(), Box<dyn std::error::Error>>(())
```

The corresponding methods are `register_named_analyzer`, `list_named_analyzers`, `set_table_field_analyzer`, `table_field_analyzer`, `get_table_analyzer`, and `drop_named_analyzer`; compatibility aliases use `create_analyzer`, `set_table_analyzer`, and `drop_analyzer`.

Construct an analyzer directly when an application needs to preview tokens or use analysis outside an engine catalog:

```rust
use uqa_analysis::{Analyzer, CharFilter, TokenFilter, Tokenizer};

let analyzer = Analyzer::new(
    Tokenizer::Standard,
    vec![TokenFilter::Lowercase, TokenFilter::PorterStem],
    vec![CharFilter::HTMLStrip],
);
assert_eq!(analyzer.analyze("<p>Running</p>")?, vec!["run"]);
# Ok::<(), Box<dyn std::error::Error>>(())
```

The process-global `uqa_analysis::register_analyzer` registry is not catalog persistence. Use the engine or SQL registration path for a persistent field assignment; otherwise a later process can reopen a field mapping whose process-local analyzer was never registered.

### Structured tokens

`Analyzer::analyze_tokens(input)` returns `AnalyzedText`. Its `tokens()` slice contains ordered `AnalysisToken` values with `term()`, `offsets()`, `position_increment()`, `position_length()`, and `is_keyword()`. Tokens emitted by the built-in tokenizers carry `Some(SourceOffsets)` in both original UTF-8 bytes and UTF-16 code units. Start the absolute position at `-1` and add each increment; the token's graph edge ends at that position plus its length. The first increment is positive, later increments may be zero, and lengths are positive. `final_offsets()` identifies the original input end, and `final_position_increment()` counts positions removed after the last emitted token.

`term()` returns `&TokenTerm`. `TokenTerm::from(text)` constructs scalar text; `from_utf16(units)` also accepts isolated surrogate units. `as_str()` returns `Some(&str)` only for scalar text, `utf16()` returns lossless units, and `into_string()` performs a checked string projection. Valid UTF-16 and string construction share the same term identity. JSON retains the existing string form for scalar terms and uses `{"utf16":[...]}` for unpaired units. `AnalyzedText::into_terms()` returns `AnalysisResult<Vec<String>>`; a non-scalar term produces `UnpairedTokenSurrogate` instead of a replacement character. Existing string-only analyzer, tokenizer, and filter entry points retain their signatures and results.

`Tokenizer::tokenize_with_offsets(input)` provides the same representation without character or token filters. `TokenFilter::filter_analyzed(previous_result)` applies one filter while preserving source and stream-end metadata. These operations do not alter catalog or transaction state. Invalid configuration, invalid source boundaries, and position overflow return an `AnalysisError`.

```rust
use uqa_analysis::standard_analyzer;

let analyzed = standard_analyzer("english").analyze_tokens("The cats and")?;
let token = &analyzed.tokens()[0];
assert_eq!(token.term(), "cat");
assert_eq!(token.offsets().unwrap().utf8, 4..8);
assert_eq!(token.position_increment(), 2);
assert_eq!(analyzed.final_position_increment(), 1);
assert_eq!(analyzed.final_offsets().utf8, 12..12);
# Ok::<(), uqa_analysis::AnalysisError>(())
```

Stop-word and length filters carry removed position increments to the next retained token or the stream end. Synonym expansions retain the original token, source range, and position length, and use increment zero; repeated configured alternatives remain repeated. N-gram and edge n-gram token filters also stack each input token's grams at its position. In contrast, the n-gram tokenizer assigns a separate position to each emitted gram. Gram filters use exact substring offsets when a token still equals its original source; after a term rewrite they retain the covering source range. Lowercase, ASCII folding, and stemming retain source ranges, and stemming leaves keyword tokens unchanged.

Generic filters also accept Nori's lossless terms. Lowercase and ASCII folding transform scalar segments while preserving isolated units. Porter stemming processes the complete token with each scalar or isolated unit as one element, and preserves keyword tokens. Length and gram filters count a surrogate pair as one scalar and each isolated unit as one element. String stop/synonym keys cannot match a non-scalar term. Token expansions and rewrites retain Korean morphology; removal filters preserve attributes observed during upstream exhaustion for later stages.

`Analyzer::analyze`, `Tokenizer::tokenize`, and `TokenFilter::filter` remain the ordered `Vec<String>` projection. Current index and query consumers use that term projection; storing and matching graph positions is tracked in the [Nori implementation plan](../../plans/0006-nori-analyzer.md).

### Character-filter source coordinates

`CharFilter::filter_with_offsets(input)` returns `FilteredText` with transformed text and mappings to the original input. Chain stages with `filter_mapped(previous_result)`. `source_offsets(range)` accepts a half-open UTF-8 byte range in the filtered text and returns its covering original UTF-8 and UTF-16 ranges; `source_offsets_utf16(range)` accepts filtered UTF-16 coordinates instead. Reversed ranges, out-of-range offsets, and boundaries inside UTF-8 characters or UTF-16 surrogate pairs return an `AnalysisError`.

```rust
use uqa_analysis::CharFilter;

let input = "<b>한&amp;🙂</b>";
let filtered = CharFilter::HTMLStrip.filter_with_offsets(input)?;
assert_eq!(filtered.as_str(), " 한&🙂 ");
let entity = filtered.source_offsets(4..5)?;
assert_eq!(&input[entity.utf8], "&amp;");
assert_eq!(entity.utf16, 4..9);
# Ok::<(), uqa_analysis::AnalysisError>(())
```

`source_covering_offsets_utf16(range)` explicitly accepts boundaries inside a surrogate pair. It projects the exact UTF-16 range through each edit map, then supplies the smallest UTF-8 range covering the affected original scalars. The returned `utf16` range keeps the exact original unit coordinates; the `utf8` range is safe to slice. An empty UTF-16 range inside a pair covers that scalar in UTF-8, while an empty range at a scalar boundary remains empty. `TextCoordinates::covering_offsets_utf16(range)` provides the same covering conversion without character filters. Existing strict methods still reject split pairs.

```rust
use uqa_analysis::CharFilter;

let filtered = CharFilter::HTMLStrip.filter_with_offsets("<b>🙂a</b>")?;
let source = filtered.source_covering_offsets_utf16(2..3)?;
assert_eq!(source.utf16, 4..5);
assert_eq!(source.utf8, 3..7);
assert_eq!(&filtered.original()[source.utf8], "🙂");
# Ok::<(), uqa_analysis::AnalysisError>(())
```

Unchanged text maps exactly. Replacements cover the full replaced source range, including regex replacements that reorder captures; inserted text maps to its original insertion boundary. Empty ranges select the following source boundary, and `final_offsets()` retains the original end even after trailing or complete deletion. These APIs are read-only analysis operations with no catalog or transaction effects. `filter` continues to return only the transformed string.

## Standalone Korean tokenization

With the optional `uqa-analysis/nori` feature, `NoriDictionary::from_bytes(bytes, limits)` loads and validates a portable dictionary, `UserDictionary::compile(source, &model, limits)` compiles optional UTF-8 noun rules, and `KoreanTokenizer::new(model, user, options)` constructs an immutable tokenizer. `tokenize(input)` returns a complete `NoriOutput`. Supply the shipped `uqa_nori_data::BUNDLE` or bytes conforming to the [bundle format](../../design/nori-bundle-format.md); loading and execution require no JVM, network, or dictionary download.

`NoriOptions` has `decompound_mode` (`none`, `discard`, or `mixed`, default `discard`), `output_unknown_unigrams` (default `false`), and `discard_punctuation` (default `true`). These options control tokenization before any filters. `none` retains the original token, `discard` emits decomposition components when present, and `mixed` retains the original graph edge plus its components. Missing decomposition differs from an explicitly empty decomposition. Punctuation retention includes reference space tokens; unknown unigram emission follows its own punctuation behavior.

User rules contain one surface followed by optional segmentation labels. Empty and comment-only sources return `None`. The compiler retains the exact source, stable UTF-16 ordering, and the first duplicate surface. Labels contribute UTF-16 lengths and may differ from the surface text or cover only its prefix; a sum longer than the surface is an error. Comment and whitespace processing follow the pinned Java implementation, including the final processed line character used for right-context selection. The compiled rules retain their model identity, and constructing a tokenizer with a different model returns an error.

Each `NoriToken` contains `term_utf16`, `start_utf16`, `end_utf16`, `position_increment`, `position_length`, `keyword`, `pos_type`, `left_pos`, `right_pos`, nullable `reading`, nullable ordered `morphemes`, and `origin` (`known`, `unknown`, or `user`). Each morpheme contains `surface_utf16` and `pos`. Offsets address the supplied UTF-16 input; absolute graph positions start at `-1` and accumulate increments. `NoriOutput` retains the final input offset even for empty output, with tokenizer final increment zero. Null reading/decomposition differs from an empty value. `NoriOutput::from_tokens(tokens, final_offset_utf16, final_position_increment)` constructs an explicit source stream. Filtered outputs also retain opaque attributes changed while the upstream stream is exhausted; keep the returned output when chaining filters because reconstructing it from its public token list or JSON loses that state.

Terms and morphemes use raw `Vec<u16>` because valid UTF-8 input and accepted user rules can cause Lucene to split a surrogate pair. For example, `🙂a 가 나` supplies two one-unit segment lengths and produces unpaired surrogate terms. `String::from_utf16` is therefore fallible. Shortened compound segmentations also use back-anchored reference offsets, which can differ from the substring that supplied the term. Retain these exact values when inspecting reference behavior; replacing invalid units loses information.

All calls are read-only and publish no catalog, index, or transaction state. Dictionary and user-rule handles are shared through `Arc`; each tokenization call owns its lattice and output. Invalid rules, dictionary mismatch, checked size/position overflow, and resource limits return an analysis or dictionary error without a partial successful result. `tokenize_controlled(input, limits, poll)` accepts a cancellation callback returning `AnalysisResult<()>`; a callback error propagates and leaves the tokenizer reusable. `tokenize_utf16(units, limits, poll)` accepts raw UTF-16 units directly.

| Limit | Default | Scope |
| --- | --- | --- |
| User-rule bytes / nonempty processed lines / surface UTF-16 units | 4 MiB / 100,000 / 65,535 | Compilation, including duplicate lines in the line budget |
| Input UTF-16 units | 16 Mi units | One call |
| Live lattice positions / candidates | 131,072 / 1,000,000 | Retained rolling lattice |
| Emitted tokens | 4,000,000 | One call |
| Output UTF-16 units | 64 Mi units | Terms, readings, morpheme surfaces, and retained terminal attributes; also bounds numeric input, intermediate coefficients, and formatting, and prospective source metadata before decomposition |

These are explicit operation bounds, not measured peak-memory or latency guarantees. Dictionary decode limits are documented with the bundle format.

```rust
use uqa_analysis::nori::{
    DictionaryLimits, KoreanAnalyzer, KoreanTokenizer, NoriDictionary, NoriOptions,
    UserDictionary, UserDictionaryLimits,
};

let model = NoriDictionary::from_bytes(uqa_nori_data::BUNDLE, DictionaryLimits::default())?;
let user = UserDictionary::compile("세종시 세종 시", &model, UserDictionaryLimits::default())?;
let tokenizer = KoreanTokenizer::new(model.clone(), user, NoriOptions::default())?;
let output = tokenizer.tokenize("세종시")?;
let terms: Result<Vec<_>, _> = output.tokens.iter()
    .map(|token| String::from_utf16(&token.term_utf16)).collect();
assert_eq!(terms?, ["세종", "시"]);
assert_eq!(output.final_offset_utf16, 3);

let analyzer = KoreanAnalyzer::new(model, None, NoriOptions::default())?;
let output = analyzer.analyze("나물은")?;
assert_eq!(String::from_utf16(&output.tokens[0].term_utf16)?, "나물");
assert_eq!(output.final_position_increment, 1);
assert_eq!(analyzer.normalize("喜悲哀歡 İ UQA")?, "喜悲哀歡 i uqa");
# Ok::<(), Box<dyn std::error::Error>>(())
```

The same example executes as a Nori module doctest. The native tokenizer is checked against the [Docker reference corpus](../../../tests/parity/nori/README.md). These are standalone Rust APIs: generic Nori configuration/compilation, JSON/SQL registration, retrieval, and binding integration remain tracked in the [implementation plan](../../plans/0006-nori-analyzer.md). The existing built-in analyzer inventory remains the one listed above.

### Korean analysis in the common token representation

`KoreanAnalyzer::analyze_tokens(input)` returns the common `AnalyzedText`, including `TokenTerm`, exact UTF-16 and covering UTF-8 source offsets, graph attributes, keyword state, Korean morphology, and complete stream end. `analyze_mapped(&filtered_text)` accepts prior character-filter output and applies source correction once. `NoriOutput::into_analyzed(&filtered_text)` converts an already computed stream; supply the same filtered text used for tokenization. A different input length, invalid token range, or invalid graph returns a typed analysis error.

`AnalysisToken::korean_morphology()` returns the optional `KoreanMorphology` with POS type, left/right POS, reading, raw morpheme units, and origin. `filtered_utf16()` retains the tokenizer's pre-correction source range when supplied. Gram filters refine that range when the unchanged token has matching source width; otherwise they retain its covering range along with the morphology. Unpaired terms remain available through `term().utf16()`. For the accepted `🙂a 가 나` rule after HTML removal, the first token retains the single `0xd83d` unit, original UTF-16 range `4..5`, and safe original UTF-8 range `3..7`.

```rust
use uqa_analysis::CharFilter;
use uqa_analysis::nori::{
    DictionaryLimits, KoreanAnalyzer, NoriDictionary, NoriOptions,
    UserDictionary, UserDictionaryLimits,
};

let model = NoriDictionary::from_bytes(uqa_nori_data::BUNDLE, DictionaryLimits::default())?;
let user = UserDictionary::compile("🙂a 가 나", &model, UserDictionaryLimits::default())?;
let analyzer = KoreanAnalyzer::with_filters(model, user, NoriOptions::default(), &[])?;
let filtered = CharFilter::HTMLStrip.filter_with_offsets("<b>🙂a</b>")?;
let output = analyzer.analyze_mapped(&filtered)?;
assert_eq!(output.tokens()[0].term().utf16().as_ref(), [0xd83d]);
assert_eq!(output.tokens()[0].offsets().unwrap().utf16, 4..5);
assert_eq!(output.tokens()[0].offsets().unwrap().utf8, 3..7);
# Ok::<(), Box<dyn std::error::Error>>(())
```

A returned common stream can be passed directly to `TokenFilter::filter_analyzed`. Keep the stream object when chaining: serialized diagnostic tokens omit the opaque exhaustion attributes. These native conversion APIs do not register a generic or SQL analyzer; resource compilation and registration remain separate implementation items.

### Korean filters and normalization

`KoreanAnalyzer::new(model, user, options)` compiles the tokenizer with the default POS stop filter, reading-form conversion, and pinned Unicode simple lowercase, in that order. `analyze(input)` returns the same lossless `NoriOutput` representation with retained morphology and stream-end gaps. `KoreanAnalyzer::with_filters(model, user, options, filters)` compiles an explicit `&[KoreanFilter]` chain; an empty chain performs tokenization only. Both constructors share immutable dictionary/user resources and keep analysis state local to each call.

| Rust filter | Serialized type | Behavior |
| --- | --- | --- |
| `KoreanFilter::PartOfSpeech { stop_tags }` | `nori_part_of_speech` | Removes tokens by left POS, carrying skipped increments to the next retained token or stream end |
| `KoreanFilter::ReadingForm` | `nori_readingform` | Replaces only the term with a present reading, including a present empty string |
| `KoreanFilter::SimpleLowercase` | `unicode_simple_lowercase` | Applies the model's pinned Java simple lowercase while retaining unpaired UTF-16 units and all metadata |
| `KoreanFilter::Number` | `nori_number` | Optionally composes Korean numbers with exact decimals and reference lookahead attributes; excluded from the default analyzer |

For the POS filter, omitted or null `stop_tags` uses `DEFAULT_STOP_TAGS`; `[]` keeps all tags. Exact tag spelling is required. The default set is `EP`, `EF`, `EC`, `ETN`, `ETM`, `IC`, `JKS`, `JKC`, `JKG`, `JKO`, `JKB`, `JKV`, `JKQ`, `JX`, `JC`, `MAG`, `MAJ`, `MM`, `SP`, `SSC`, `SSO`, `SC`, `SE`, `XPN`, `XSA`, `XSN`, `XSV`, `UNA`, `NA`, and `VSV`. Unknown filter properties or tags fail deserialization. This filter vocabulary is exposed by the standalone Rust `KoreanFilter` type; it is not yet accepted by generic analyzer registration or SQL configuration.

`KoreanFilter::apply(output, &model)` applies one stage to a complete output; `apply_controlled(output, &model, limits, poll)` also enforces token/output bounds and cancellation. POS, reading-form, and lowercase filters preserve retained tokens' offsets, position lengths, keyword state, origin, and nullable morphology. POS filtering adds increments with checked arithmetic, including final skipped positions. Reading conversion and lowercase do not modify reading or morpheme metadata.

`KoreanAnalyzer::normalize(input)` returns one `String` after simple lowercase of the complete input. It runs neither tokenization, user-rule matching, POS stops, nor reading conversion, and remains independent of an explicitly configured analysis chain. Thus `喜悲哀歡 İ UQA` normalizes to `喜悲哀歡 i uqa`. Existing generic `lowercase` retains its Rust full-lowercase behavior. `normalize_controlled` adds the same limits/cancellation arguments; `normalize_utf16(units, limits, poll)` exposes lossless simple normalization of raw units. A custom profile whose lowercase mapping changes UTF-16 width returns an error because the reference filter writes in place.

The default constructor uses the default POS/readings/lowercase chain with `NoriOptions::default()` matching Lucene's `KoreanAnalyzer` defaults. Explicit punctuation and unigram options remain those of the tokenizer, and explicit filter order remains observable. These operations change no catalog or index state. Limits, cancellation, invalid profiles, and position overflow return errors without publishing partial successful output; the compiled analyzer remains reusable.

### Optional Korean number composition

`KoreanFilter::Number` composes adjacent numeric tokens and intervening decimal/grouping punctuation. It uses exact decimal arithmetic for Arabic, fullwidth, and Korean digits and powers through `해`, without floating-point rounding or SQL `NUMERIC` precision limits. It preserves Lucene's keyword protection, stacked-token fallthrough, aborted-composition state, and stream-end behavior. Resource limits and cancellation remain errors; malformed decimals retain their original units.

The filter changes the composed term and covering offsets but inherits the shared attributes present after lookahead, including POS, reading, keyword, position increment, and position length. A later filter therefore observes the lookahead metadata on the normalized number. For example, composing before the default POS filter can remove a number that inherited the following space's `SP` tag. Reading conversion after number composition can replace the numeric term with the following word's reading. An exhausted upstream POS filter can also change these attributes without emitting another token.

For a concrete numeric chain, retain punctuation, remove only `SP` tokens, then compose numbers. The following chain produces `3200`, `원`, and `157` from `３．２천 원 15,7`. Removing punctuation in the tokenizer changes `３．２천` to `32000`; replacing this explicitly ordered chain changes its reference behavior.

```rust
use uqa_analysis::nori::{
    DecompoundMode, DictionaryLimits, KoreanAnalyzer, KoreanFilter,
    NoriDictionary, NoriOptions, POSTag,
};

let model = NoriDictionary::from_bytes(uqa_nori_data::BUNDLE, DictionaryLimits::default())?;
let numbers = KoreanAnalyzer::with_filters(model, None, NoriOptions {
    decompound_mode: DecompoundMode::None,
    discard_punctuation: false,
    ..NoriOptions::default()
}, &[
    KoreanFilter::PartOfSpeech { stop_tags: Some(vec![POSTag::SP]) },
    KoreanFilter::Number,
])?;
let output = numbers.analyze("３．２천 원 15,7")?;
let terms: Result<Vec<_>, _> = output.tokens.iter()
    .map(|token| String::from_utf16(&token.term_utf16)).collect();
assert_eq!(terms?, ["3200", "원", "157"]);
# Ok::<(), Box<dyn std::error::Error>>(())
```

`normalize_number(input)` is a separate direct helper and does not tokenize or require a dictionary. It normalizes the successfully parsed numeric prefix, so `12원` becomes `12`, while a malformed decimal such as `1.2.3` or absent numeric prefix retains the complete input. `normalize_number_utf16(units, limits, poll)` provides the same operation for raw UTF-16 with cancellation. This prefix API differs from the filter's whole-token numeric eligibility. Both forms leave the default Korean analyzer and its simple-lowercase `normalize` operation unchanged.

## Python, Node.js, and browser WASM

Every binding can create, bind, inspect, search with, and drop analyzers by executing the SQL functions in this chapter. Python exposes `list_named_analyzers()`. Node.js and browser WASM expose `listNamedAnalyzers()`; these direct methods list custom engine-catalog names, while SQL `list_analyzers()` also includes built-ins. Direct construction from `CharFilter`, `Tokenizer`, and `TokenFilter` is a Rust API, so other bindings define the pipeline as JSON passed to SQL.

## File-backed synonyms

The synonym filter accepts a reloadable file path:

```json
{
  "type": "synonym",
  "synonyms_path": "/srv/uqa/synonyms.txt"
}
```

The file format accepts comments, one-way mappings, and equivalent groups:

```text
# One-way expansion
car => automobile, vehicle

# Every member expands to the others
fast, quick, rapid
```

Registration reads the file once to reject a missing or unreadable path. Analysis reads it again on every execution, so edits are visible without re-registering the analyzer and later deletion or permission loss becomes an explicit indexing or search error. The catalog stores the path, not a copy of the file; every process and reopen environment must provide the same resource. Use inline synonyms when the catalog must be self-contained.

## Validation and operational rules

- Registration parses JSON, normalizes supported string shorthand, validates regular expressions and gram bounds, and validates a configured synonym file before publishing catalog state.
- Analyzer names must be non-empty when resolved. Use custom names that do not collide with `standard`, `whitespace`, `standard_cjk`, or `keyword`.
- A field assignment requires an existing table, a `TEXT` column, and a physical GIN field.
- Index-time analysis failure aborts the document write without publishing partial row or posting state.
- Persistent analyzer definitions and assignments are restored during engine reopen; an invalid catalog configuration makes reopen fail explicitly.
- SQL `uqa_highlight` currently uses the built-in English `standard` analyzer and does not inherit a table-field analyzer. The typed `uqa_analysis::highlight` API accepts an explicit analyzer.

## Related documentation

- [Analyzer SQL](../sql/05-analyzers.md)
- [Analyzer pipeline tutorial](../tutorials/03-analyzer-pipelines.md)
- [Search and ranking](05-search-and-ranking.md)
- [Analyzer internals](../internals/04-analyzer-pipeline.md)
