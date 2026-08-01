//! WAND/BMW planning, block-max lifecycle, and profiled text leaves.

use super::{
    block_max_scorer_fingerprint, raw_bm25_params, search_stats_for_terms, storage_sql_error, Arc,
    BM25Scorer, BTreeSet, BlockMaxIndex, BlockMaxWANDScorer, Engine, Instant, InvertedIndex,
    OperatorTree, PostingList, SQLError, ScoredEntry, ScoringMode, TextScoringMode,
    TextSearchAlgorithm, TextSearchProfile, TextTopKPlan, TextTopKStrategy, WANDQuery, WANDScorer,
    WANDStats, DEFAULT_BLOCK_SIZE,
};

impl Engine {
    pub(crate) fn plan_text_top_k_tree(
        &self,
        table: &str,
        field: &str,
        query: &str,
        mode: &ScoringMode,
        scoring: TextScoringMode,
        top_k: usize,
    ) -> Result<OperatorTree, SQLError> {
        let capabilities = self.text_top_k_capabilities(table, field, query, mode)?;
        Ok(uqa_planner::plan_text_top_k(
            OperatorTree::Term {
                query: query.to_string(),
                field: Some(field.to_string()),
                scoring: Some(scoring),
                top_k: None,
            },
            top_k,
            capabilities,
        ))
    }

    /// Materialize scorer-versioned block bounds for one text field. `SQLite`
    /// persists them across reopen; in-memory indexes return `false` and the
    /// planner continues to choose plain WAND.
    pub fn rebuild_text_block_max(
        &self,
        table: &str,
        field: &str,
        mode: &ScoringMode,
    ) -> Result<bool, SQLError> {
        self.validate_text_search_field(table, field)?;
        self.with_implicit_transaction(|engine| {
            let Some(table_state) = engine
                .try_table(table)
                .map_err(|error| storage_sql_error("resolve text-search table", error))?
            else {
                return Err(SQLError::UnknownTable(table.to_string()));
            };
            let mut index = table_state.inverted_index.write();
            let stats = Arc::new(
                index
                    .field_stats_scalar(field)
                    .map_err(|error| storage_sql_error("read field statistics", error))?,
            );
            let params = raw_bm25_params(mode);
            params
                .validate()
                .map_err(|error| SQLError::TypeMismatch(error.to_string()))?;
            let fingerprint = block_max_scorer_fingerprint(params, stats.as_ref());
            let scorer = BM25Scorer::new(params, stats);
            index
                .rebuild_persisted_block_max(field, &scorer, &fingerprint)
                .map_err(|error| storage_sql_error("rebuild persisted block-max scores", error))
        })
    }

    pub(super) fn load_block_max_index(
        index: &dyn InvertedIndex,
        table: &str,
        field: &str,
        analyzed_terms: &[String],
        posting_lists: &[PostingList],
        fingerprint: &str,
    ) -> Result<Option<BlockMaxIndex>, SQLError> {
        let mut block_max = BlockMaxIndex::new(DEFAULT_BLOCK_SIZE)
            .map_err(|error| storage_sql_error("create block-max index", error))?;
        let mut checked = BTreeSet::new();
        for (term, posting) in analyzed_terms.iter().zip(posting_lists) {
            if posting.is_empty() || !checked.insert(term) {
                continue;
            }
            let Some(scores) = index
                .persisted_block_max_scores(field, term, fingerprint)
                .map_err(|error| storage_sql_error("read persisted block-max scores", error))?
            else {
                return Ok(None);
            };
            if scores.len() != posting.len().div_ceil(DEFAULT_BLOCK_SIZE) {
                return Ok(None);
            }
            block_max
                .set_block_maxes(table, field, term, scores)
                .map_err(|error| storage_sql_error("load block-max scores", error))?;
        }
        Ok(Some(block_max))
    }

    pub(super) fn score_text_top_k(
        index: &dyn InvertedIndex,
        table: &str,
        field: &str,
        analyzed_terms: &[String],
        mode: &ScoringMode,
        plan: TextTopKPlan,
    ) -> Result<(Vec<ScoredEntry>, WANDStats, TextSearchAlgorithm), SQLError> {
        let posting_lists = index
            .get_posting_lists_bulk(field, analyzed_terms)
            .map_err(|error| storage_sql_error("read text postings", error))?;
        let doc_freqs = posting_lists
            .iter()
            .map(|posting| {
                u64::try_from(posting.len()).map_err(|_| {
                    SQLError::Internal("text-search document frequency exceeds u64".into())
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let stats = Arc::new(
            search_stats_for_terms(index, field, analyzed_terms, &doc_freqs)
                .map_err(|error| storage_sql_error("read field statistics", error))?,
        );
        let scorer = Self::build_text_scorer(mode, stats.clone(), analyzed_terms.len())?;
        let wand_query = WANDQuery::new(
            posting_lists.clone(),
            vec![scorer; analyzed_terms.len()],
            vec![field.to_string(); analyzed_terms.len()],
            analyzed_terms.to_vec(),
            plan.k,
        )
        .map_err(|error| storage_sql_error("build WAND query", error))?;

        let (result, algorithm) = match plan.strategy {
            TextTopKStrategy::Wand => (
                WANDScorer::new(&wand_query, Some(index))
                    .score_top_k()
                    .map_err(|error| storage_sql_error("execute WAND", error))?,
                TextSearchAlgorithm::Wand,
            ),
            TextTopKStrategy::BlockMaxWand => {
                let fingerprint =
                    block_max_scorer_fingerprint(raw_bm25_params(mode), stats.as_ref());
                if let Some(block_max) = Self::load_block_max_index(
                    index,
                    table,
                    field,
                    analyzed_terms,
                    &posting_lists,
                    &fingerprint,
                )? {
                    (
                        BlockMaxWANDScorer::new(&wand_query, Some(index), &block_max, table)
                            .score_top_k()
                            .map_err(|error| storage_sql_error("execute Block-Max WAND", error))?,
                        TextSearchAlgorithm::BlockMaxWand,
                    )
                } else {
                    // A concurrent or transactional posting mutation can
                    // invalidate blocks after planning. Exact WAND is the safe
                    // physical fallback; stale bounds are never consumed.
                    (
                        WANDScorer::new(&wand_query, Some(index))
                            .score_top_k()
                            .map_err(|error| storage_sql_error("execute WAND fallback", error))?,
                        TextSearchAlgorithm::Wand,
                    )
                }
            }
        };
        let entries = Self::rank_scored_entries_top_k(
            result.top_k.iter().map(ScoredEntry::from_entry).collect(),
            plan.k,
        );
        Ok((entries, result.stats, algorithm))
    }

    pub(super) fn search_leaf_profiled(
        &self,
        table: &str,
        field: &str,
        query: &str,
        mode: &ScoringMode,
        top_k: usize,
        physical_top_k: Option<TextTopKPlan>,
    ) -> Result<TextSearchProfile, SQLError> {
        let started = Instant::now();
        let Some(t) = self
            .try_table(table)
            .map_err(|error| storage_sql_error("resolve text-search table", error))?
        else {
            return Err(SQLError::UnknownTable(table.to_string()));
        };
        let index = t.inverted_index.read();
        let analyzer = index.get_search_analyzer(field);
        let analyzed_terms = analyzer
            .analyze(query)
            .map_err(|error| storage_sql_error("analyze text query", error))?;
        if analyzed_terms.is_empty() {
            return Ok(TextSearchProfile {
                entries: Vec::new(),
                algorithm: TextSearchAlgorithm::Exhaustive,
                scored_candidates: 0,
                total_candidates: 0,
                cursor_advances: 0,
                skip_rate: 0.0,
                elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
            });
        }

        if let Some(plan) = physical_top_k {
            let (entries, stats, algorithm) =
                Self::score_text_top_k(index.as_ref(), table, field, &analyzed_terms, mode, plan)?;
            return Ok(TextSearchProfile {
                entries,
                algorithm,
                scored_candidates: stats.scored,
                total_candidates: stats.total_candidates,
                cursor_advances: stats.cursor_advances,
                skip_rate: stats.skip_rate(),
                elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
            });
        }

        // Walk the postings once per term occurrence, keeping only
        // `(doc_id, term_freq)` and the document frequencies required by the
        // scorer. Single-term queries avoid the per-document term map.
        let entries = if analyzed_terms.len() == 1 {
            Self::score_single_text_term(index.as_ref(), field, &analyzed_terms, mode)?
        } else {
            Self::score_multiple_text_terms(index.as_ref(), field, &analyzed_terms, mode)?
        };
        let total_candidates = u64::try_from(entries.len())
            .map_err(|_| SQLError::Internal("text candidate count exceeds u64".into()))?;
        Ok(TextSearchProfile {
            entries: Self::rank_scored_entries_top_k(entries, top_k),
            algorithm: TextSearchAlgorithm::Exhaustive,
            scored_candidates: total_candidates,
            total_candidates,
            cursor_advances: 0,
            skip_rate: 0.0,
            elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
        })
    }

    /// Physical text-search leaf. Only [`crate::operator_tree_bridge::EngineDriver`]
    /// calls this; public callers enter through [`Self::search`] below.
    pub(crate) fn search_leaf(
        &self,
        table: &str,
        field: &str,
        query: &str,
        mode: &ScoringMode,
        top_k: usize,
        physical_top_k: Option<TextTopKPlan>,
    ) -> Result<Vec<ScoredEntry>, SQLError> {
        Ok(self
            .search_leaf_profiled(table, field, query, mode, top_k, physical_top_k)?
            .entries)
    }
}
