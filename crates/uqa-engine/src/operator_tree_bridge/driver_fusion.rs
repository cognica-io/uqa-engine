//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Probability, evidence, staged, progressive, and deep-fusion execution.

use super::{
    collect_graph_names, combine_signal_priors, deep_runtime_gating, fuse_signal_batches_with,
    fuse_signals_with, lower_deep_batch_norm, lower_deep_conv, lower_deep_dense,
    lower_deep_dropout, lower_deep_pool, operator_execution_error, scored_term_count,
    scored_to_posting_list, sql, static_operator, BTreeSet, BayesianEvidenceFusionOperator, DocId,
    DriverExecution, DriverResult, EngineDriver, ExternalPriorMode, GatingSpec,
    GraphNeighborSnapshot, MultiStageCutoff, MultiStageEntry, OperatorTree, Payload,
    PositiveEvidencePoolExecution, PostingEntry, PostingList, RobustPositiveEvidencePoolOperator,
    SQLError, ScalarExpr, ScoredEntry, StaticPostingList, StorageBackendError, TextScoringMode,
    Value,
};

impl EngineDriver<'_> {
    pub(super) fn execute_facet_vector(
        &self,
        vector_op: &OperatorTree,
        facet_field: &str,
    ) -> DriverResult<PostingList> {
        let vec_pl = self.execute_posting_node(vector_op)?;
        self.facet_vector_inline(&vec_pl, facet_field)
    }

    pub(super) fn execute_prob_bool_fusion(
        &self,
        signals: &[OperatorTree],
        mode: uqa_operators::ProbBoolMode,
    ) -> DriverResult<PostingList> {
        use uqa_operators::base::Operator;
        use uqa_operators::{HybridProbBoolMode, ProbBoolFusionOperator};
        if signals.is_empty() {
            return Err(SQLError::TypeMismatch(
                "ProbBoolFusion requires at least one signal".to_string(),
            ));
        }
        // Pre-execute every child through the driver, then wrap the
        // results in static signal operators so the fusion operator can
        // consume them without taking a back-reference into the driver.
        let signal_ops: Vec<std::sync::Arc<dyn Operator>> = self
            .execute_posting_branches(signals)?
            .into_iter()
            .map(|pl| -> std::sync::Arc<dyn Operator> {
                std::sync::Arc::new(StaticPostingList { pl })
            })
            .collect();
        let mode = match mode {
            uqa_operators::ProbBoolMode::And => HybridProbBoolMode::And,
            uqa_operators::ProbBoolMode::Or => HybridProbBoolMode::Or,
        };
        let op = ProbBoolFusionOperator::new(signal_ops, mode);
        op.execute(&self.bridge_context()?)
            .map_err(|error| operator_execution_error("ProbBoolFusion", error))
    }

    pub(super) fn execute_multi_field_search(
        &self,
        fields: &[String],
        queries: &[String],
        weights: Option<&[f64]>,
    ) -> DriverResult<PostingList> {
        // Delegate to the row-function implementation like the other
        // leaf nodes, so every lowering of `multi_field_match` shares
        // one pad, one per-field analyzer choice, and one stats source.
        if fields.len() != queries.len() {
            return Err(SQLError::Internal(format!(
                "multi-field IR has {} fields but {} queries",
                fields.len(),
                queries.len()
            )));
        }
        let all_queries_equal = queries
            .first()
            .is_none_or(|first| queries.iter().all(|query| query == first));
        let mut args = Vec::new();
        if all_queries_equal {
            args.extend(fields.iter().cloned().map(ScalarExpr::Column));
            if let Some(query) = queries.first() {
                args.push(ScalarExpr::Literal(Value::Str(query.clone())));
            }
        } else {
            if weights.is_some() {
                return Err(SQLError::Internal(
                    "multi-field IR cannot attach one weight vector to distinct per-field queries"
                        .to_string(),
                ));
            }
            for (field, query) in fields.iter().zip(queries) {
                args.push(ScalarExpr::Column(field.clone()));
                args.push(ScalarExpr::Literal(Value::Str(query.clone())));
            }
        }
        if let Some(weights) = weights {
            args.extend(
                weights
                    .iter()
                    .map(|weight| ScalarExpr::Literal(Value::Float(*weight))),
            );
        }
        let run = match self.execution {
            DriverExecution::Public => sql::run_multi_field_match_public,
            DriverExecution::InExecution => sql::run_multi_field_match_in_execution,
        };
        run(self.engine, self.table, &args, self.params).map(|rows| scored_to_posting_list(&rows))
    }

    pub(super) fn execute_bayesian_match_with_prior(
        &self,
        field: &str,
        query: &str,
        prior_field: &str,
        mode: ExternalPriorMode,
    ) -> DriverResult<PostingList> {
        let args = vec![
            ScalarExpr::Column(field.to_string()),
            ScalarExpr::Literal(Value::Str(query.to_string())),
            ScalarExpr::Column(prior_field.to_string()),
            ScalarExpr::Literal(Value::Str(
                match mode {
                    ExternalPriorMode::Authority => "authority",
                    ExternalPriorMode::Recency => "recency",
                }
                .to_string(),
            )),
        ];
        let run = match self.execution {
            DriverExecution::Public => sql::run_bayesian_match_with_prior_public,
            DriverExecution::InExecution => sql::run_bayesian_match_with_prior_in_execution,
        };
        run(self.engine, self.table, &args, self.params).map(|rows| scored_to_posting_list(&rows))
    }

    pub(super) fn execute_calibrated_vector_match(
        &self,
        field: &str,
        query_vector: &[f32],
        k: usize,
        threshold: Option<f64>,
    ) -> DriverResult<PostingList> {
        self.require_vector_query(field, query_vector)?;
        let mut args = vec![
            ScalarExpr::Literal(Value::Str(field.to_string())),
            ScalarExpr::Array(
                query_vector
                    .iter()
                    .map(|value| ScalarExpr::Literal(Value::Float(f64::from(*value))))
                    .collect(),
            ),
            ScalarExpr::Literal(Value::Int(i64::try_from(k).map_err(|_| {
                SQLError::TypeMismatch(format!("calibrated vector k is too large: {k}"))
            })?)),
        ];
        if let Some(threshold) = threshold {
            args.push(ScalarExpr::Literal(Value::Float(threshold)));
        }
        sql::run_calibrated_vector_match_public(self.engine, self.table, &args, self.params)
            .map(|rows| scored_to_posting_list(&rows))
    }

    pub(super) fn execute_deep_predict(&self, model: &str) -> DriverResult<PostingList> {
        let scores = self
            .engine
            .deep_predict_leaf(model)?
            .ok_or_else(|| SQLError::Unsupported(format!("unknown model {model:?}")))?;
        Ok(PostingList::from_unsorted(
            scores
                .into_iter()
                .map(|(doc_id, score)| PostingEntry::new(doc_id, Payload::with_score(score)))
                .collect(),
        ))
    }

    pub(super) fn execute_prob_not(
        &self,
        signal: &OperatorTree,
        default_prob: f64,
    ) -> DriverResult<PostingList> {
        use uqa_operators::base::Operator;
        use uqa_operators::ProbNotOperator;
        let signal_pl = self.execute_posting_node(signal)?;
        let signal_op: std::sync::Arc<dyn Operator> =
            std::sync::Arc::new(StaticPostingList { pl: signal_pl });
        let op = ProbNotOperator::new(signal_op, default_prob);
        op.execute(&self.bridge_context()?)
            .map_err(|error| operator_execution_error("ProbNot", error))
    }

    pub(super) fn execute_index_scan(
        &self,
        index_name: &str,
        field: &str,
        predicate: &uqa_core::Predicate,
    ) -> DriverResult<PostingList> {
        self.require_column(field)?;
        let index = self
            .engine
            .catalog_index(index_name)
            .map_err(|error| operator_execution_error("resolve physical index", error))?
            .ok_or_else(|| {
                SQLError::Unsupported(format!("unknown physical index {index_name:?}"))
            })?;
        let resolved_table = self
            .engine
            .resolve_table_name(self.table)
            .map_err(|error| operator_execution_error("resolve index table", error))?
            .unwrap_or_else(|| self.table.to_string());
        if index.table_name != resolved_table {
            return Err(SQLError::TypeMismatch(format!(
                "index {index_name:?} belongs to table {:?}, not {:?}",
                index.table_name, self.table
            )));
        }
        if !index.index_type.eq_ignore_ascii_case("btree") {
            return Err(SQLError::TypeMismatch(format!(
                "IndexScan requires a btree index, but {index_name:?} is {:?}",
                index.index_type
            )));
        }
        let columns: Vec<String> = serde_json::from_str(&index.columns_json).map_err(|error| {
            SQLError::Internal(format!(
                "index {index_name:?} has malformed column metadata: {error}"
            ))
        })?;
        if columns.first().is_none_or(|column| column != field) {
            return Err(SQLError::TypeMismatch(format!(
                "index {index_name:?} does not cover leading field {field:?}"
            )));
        }
        self.engine
            .value_index_scan(self.table, field, predicate)?
            .ok_or_else(|| {
                SQLError::Unsupported(format!(
                    "index {index_name:?} cannot evaluate predicate {predicate:?}"
                ))
            })
    }

    pub(super) fn execute_vector_exclusion(
        &self,
        positive: &OperatorTree,
        negative: &OperatorTree,
    ) -> DriverResult<PostingList> {
        let pos = self.execute_posting_node(positive)?;
        let neg = self.execute_posting_node(negative)?;
        let neg_ids: BTreeSet<DocId> = neg.entries().iter().map(|e| e.doc_id).collect();
        let mut entries: Vec<PostingEntry> = Vec::new();
        for entry in pos.entries() {
            if !neg_ids.contains(&entry.doc_id) {
                entries.push(entry.clone());
            }
        }
        Ok(PostingList::from_sorted_unchecked(entries))
    }

    pub(super) fn execute_positive_evidence_pool(
        &self,
        execution: PositiveEvidencePoolExecution<'_>,
    ) -> DriverResult<PostingList> {
        use uqa_operators::base::Operator;

        let PositiveEvidencePoolExecution {
            signals,
            alpha,
            gating,
            weights,
            logit_min,
            logit_max,
            adaptive_weights,
        } = execution;

        if signals.is_empty() {
            return Err(SQLError::TypeMismatch(
                "RobustPositiveEvidencePool requires at least one signal".to_string(),
            ));
        }
        let mut signal_ops: Vec<std::sync::Arc<dyn Operator>> = Vec::with_capacity(signals.len());
        let mut signal_priors: Vec<f64> = Vec::new();
        for (pl, prior) in self.execute_fusion_signal_branches(signals)? {
            signal_ops.push(std::sync::Arc::new(StaticPostingList { pl }));
            if let Some(prior) = prior {
                signal_priors.push(prior);
            }
        }
        let logit_gating = match gating {
            GatingSpec::Softplus => uqa_fusion::LogitGating::Softplus,
            GatingSpec::Pass => uqa_fusion::LogitGating::Pass,
            GatingSpec::Sigmoid { .. } => uqa_fusion::LogitGating::Sigmoid,
            GatingSpec::ReLU => uqa_fusion::LogitGating::ReLU,
            GatingSpec::Swish => uqa_fusion::LogitGating::Swish,
            GatingSpec::Gelu => uqa_fusion::LogitGating::Gelu,
        };
        let mut operator =
            RobustPositiveEvidencePoolOperator::new(signal_ops, alpha).with_gating(logit_gating);
        if adaptive_weights {
            operator = operator.with_adaptive_weights();
        }
        if let Some(base_rate) = combine_signal_priors(&signal_priors) {
            operator = operator.with_base_rate(base_rate);
        }
        if let Some(weights) = weights {
            operator = operator.with_weights(weights.to_vec());
        }
        if let (Some(logit_min), Some(logit_max)) = (logit_min, logit_max) {
            operator = operator.with_logit_normalization(logit_min.to_vec(), logit_max.to_vec());
        }
        operator
            .execute(&self.bridge_context()?)
            .map_err(|error| operator_execution_error("RobustPositiveEvidencePool", error))
    }

    pub(super) fn execute_bayesian_evidence_fusion(
        &self,
        signals: &[OperatorTree],
        explicit_base_rate: Option<f64>,
    ) -> DriverResult<PostingList> {
        use uqa_operators::base::Operator;

        if signals.is_empty() {
            return Err(SQLError::TypeMismatch(
                "BayesianEvidenceFusion requires at least one signal".to_string(),
            ));
        }
        let mut signal_ops: Vec<std::sync::Arc<dyn Operator>> = Vec::with_capacity(signals.len());
        let mut signal_priors = Vec::new();
        for (posting, prior) in self.execute_fusion_signal_branches(signals)? {
            signal_ops.push(std::sync::Arc::new(StaticPostingList { pl: posting }));
            if let Some(prior) = prior {
                signal_priors.push(prior);
            }
        }
        let base_rate = explicit_base_rate
            .or_else(|| combine_signal_priors(&signal_priors))
            .unwrap_or(0.5);
        BayesianEvidenceFusionOperator::new(signal_ops, base_rate)
            .execute(&self.bridge_context()?)
            .map_err(|error| operator_execution_error("BayesianEvidenceFusion", error))
    }

    /// Execute independent probability/evidence signals through the shared branch executor while preserving declaration order for weights and per-signal normalization. The fusion itself remains deterministic.
    fn execute_fusion_signal_branches(
        &self,
        signals: &[OperatorTree],
    ) -> DriverResult<Vec<(PostingList, Option<f64>)>> {
        let workers: Vec<_> = signals
            .iter()
            .map(|signal| || self.execute_fusion_signal(signal))
            .collect();
        self.parallel
            .execute_branches(&workers)
            .into_iter()
            .collect()
    }

    /// Execute a combination child at the typed probability/evidence boundary: the
    /// signal contributes prior-free evidence and reports the corpus
    /// relevance prior it would otherwise have folded in, so the
    /// fusion can apply that prior exactly once.
    pub(super) fn execute_fusion_signal(
        &self,
        signal: &OperatorTree,
    ) -> DriverResult<(PostingList, Option<f64>)> {
        match signal {
            OperatorTree::BayesianScore { source, field } => {
                let params = match field.as_deref() {
                    Some(field) => self.bayesian_params_for(field)?,
                    None => uqa_scoring::BayesianBM25Params::default(),
                }
                .scaled_for_query_terms(scored_term_count(source));
                let prior = (params.base_rate > 0.0).then_some(params.base_rate);
                let evidence_params = params.evidence_params();
                let raw = self.execute_posting_node(source)?;
                let evidence = raw.with_scores(|entry| {
                    uqa_scoring::sigmoid(
                        evidence_params.alpha * (entry.payload.score - evidence_params.beta),
                    )
                });
                Ok((evidence, prior))
            }
            OperatorTree::Term {
                query,
                field,
                scoring: Some(TextScoringMode::BayesianBM25),
                top_k: None,
            } => self.execute_bayesian_term_evidence(query, field.as_deref()),
            OperatorTree::CosineProbability(source) => self
                .execute_cosine_evidence(source)
                .map(|posting| (posting, None)),
            other => self
                .execute_posting_node(other)
                .map(|posting| (posting, None)),
        }
    }

    /// Execute a Bayesian term as prior-free evidence while resolving each field calibration exactly once for both scoring and the fusion prior.
    fn execute_bayesian_term_evidence(
        &self,
        query: &str,
        field: Option<&str>,
    ) -> DriverResult<(PostingList, Option<f64>)> {
        if let Some(field) = field {
            self.engine.validate_text_search_field(self.table, field)?;
            let params = self.bayesian_params_for(field)?;
            let prior = (params.base_rate > 0.0).then_some(params.base_rate);
            let mode = crate::ScoringMode::BayesianBM25(params.evidence_params());
            let rows =
                self.engine
                    .search_leaf(self.table, field, query, &mode, usize::MAX, None)?;
            return Ok((scored_to_posting_list(&rows), prior));
        }
        let fields = self.engine.fts_fields_for_table(self.table)?;
        if fields.is_empty() {
            return Err(SQLError::TypeMismatch(format!(
                "text search: table `{}` has no text-indexed columns",
                self.table
            )));
        }
        let mut by_document = std::collections::BTreeMap::<DocId, f64>::new();
        let mut priors = Vec::with_capacity(fields.len());
        for field in fields {
            let params = self.bayesian_params_for(&field)?;
            if params.base_rate > 0.0 {
                priors.push(params.base_rate);
            }
            let mode = crate::ScoringMode::BayesianBM25(params.evidence_params());
            for entry in
                self.engine
                    .search_leaf(self.table, &field, query, &mode, usize::MAX, None)?
            {
                by_document
                    .entry(entry.doc_id)
                    .and_modify(|score| *score = score.max(entry.score))
                    .or_insert(entry.score);
            }
        }
        let rows = by_document
            .into_iter()
            .map(|(doc_id, score)| ScoredEntry { doc_id, score })
            .collect::<Vec<_>>();
        Ok((
            scored_to_posting_list(&rows),
            combine_signal_priors(&priors),
        ))
    }

    /// Query-pool vector evidence: fit the two-Gaussian score transform on
    /// the source's selected cosine similarities and emit unit-interval,
    /// prior-free evidence. This is a ranking heuristic, not a reusable
    /// held-out calibration model.
    pub(super) fn execute_cosine_evidence(
        &self,
        source: &OperatorTree,
    ) -> DriverResult<PostingList> {
        let pl = self.execute_posting_node(source)?;
        let distances: Vec<f64> = pl.iter().map(|e| 1.0 - e.payload.score).collect();
        let calibrated = match uqa_operators::fit_pool_calibration(
            &distances,
            uqa_operators::RelevantSampleSplit::default(),
            0.5,
        )
        .map_err(|error| operator_execution_error("CosineEvidence", error))?
        {
            Some(transform) => {
                let mut calibrated = Vec::with_capacity(pl.len());
                for entry in &pl {
                    let score = transform
                        .calibrate_one(1.0 - entry.payload.score)
                        .map_err(|error| {
                            operator_execution_error(
                                "CosineEvidence",
                                StorageBackendError::Other(error.to_string()),
                            )
                        })?
                        .clamp(1e-6, 1.0 - 1e-6);
                    calibrated.push(PostingEntry::new(
                        entry.doc_id,
                        Payload {
                            score,
                            ..entry.payload.clone()
                        },
                    ));
                }
                PostingList::from_sorted_unchecked(calibrated)
            }
            None => pl.with_scores(|_| 0.5),
        };
        Ok(calibrated)
    }

    pub(super) fn execute_cosine_probability(
        &self,
        source: &OperatorTree,
    ) -> DriverResult<PostingList> {
        // Lift cosine similarities in `[-1, 1]` onto the (0, 1)
        // probability scale via `(1 + s) / 2`. The source is already driven,
        // so this path skips the operator trait wrapper
        // through the engine. Standalone `knn_match` keeps this
        // Definition 7.1.2 map; fusion contexts route through
        // [`Self::execute_cosine_evidence`] instead.
        use uqa_scoring::cosine_to_probability;
        let pl = self.execute_posting_node(source)?;
        Ok(pl.with_scores(|e| cosine_to_probability(e.payload.score)))
    }

    pub(super) fn execute_bayesian_score(
        &self,
        source: &OperatorTree,
        field: Option<&str>,
    ) -> DriverResult<PostingList> {
        let raw = self.execute_posting_node(source)?;
        let params = match field {
            Some(field) => self.bayesian_params_for(field)?,
            None => uqa_scoring::BayesianBM25Params::default(),
        }
        .scaled_for_query_terms(scored_term_count(source));
        Ok(raw.with_scores(|entry| {
            uqa_scoring::sigmoid(params.alpha * (entry.payload.score - params.beta))
        }))
    }

    pub(super) fn execute_attention_fusion(
        &self,
        signals: &[OperatorTree],
        attention: &uqa_operators::tree::AttentionRef,
        query_features: &[f64],
    ) -> DriverResult<PostingList> {
        if signals.is_empty() {
            return Err(SQLError::TypeMismatch(
                "AttentionFusion requires at least one signal".to_string(),
            ));
        }
        let features = self.attention_query_features(signals, query_features)?;
        attention
            .validate_inputs(signals.len(), features.len())
            .map_err(|error| SQLError::TypeMismatch(format!("AttentionFusion: {error}")))?;
        let posting_lists = self.execute_posting_branches(signals)?;
        fuse_signal_batches_with(&posting_lists, |probabilities| {
            attention
                .fuse_batch(probabilities, &features)
                .map_err(|error| SQLError::TypeMismatch(format!("AttentionFusion: {error}")))
        })
    }

    pub(super) fn execute_learned_fusion(
        &self,
        signals: &[OperatorTree],
        learned: &uqa_operators::tree::LearnedFusionRef,
    ) -> DriverResult<PostingList> {
        if signals.is_empty() {
            return Err(SQLError::TypeMismatch(
                "LearnedFusion requires at least one signal".to_string(),
            ));
        }
        learned
            .validate_inputs(signals.len())
            .map_err(|error| SQLError::TypeMismatch(format!("LearnedFusion: {error}")))?;
        let posting_lists = self.execute_posting_branches(signals)?;
        fuse_signals_with(&posting_lists, |probs| {
            learned
                .fuse(probs)
                .map_err(|error| SQLError::TypeMismatch(format!("LearnedFusion: {error}")))
        })
    }

    pub(super) fn execute_multi_stage(
        &self,
        stages: &[MultiStageEntry],
    ) -> DriverResult<PostingList> {
        if stages.is_empty() {
            return Err(SQLError::TypeMismatch(
                "MultiStage requires at least one stage".to_string(),
            ));
        }
        let mut current: Option<PostingList> = None;
        for stage in stages {
            let stage_result = self.execute_posting_node(&stage.child)?;
            let mut entries: Vec<PostingEntry> = if let Some(prior) = &current {
                let prior_ids: BTreeSet<DocId> = prior.entries().iter().map(|e| e.doc_id).collect();
                stage_result
                    .entries()
                    .iter()
                    .filter(|entry| prior_ids.contains(&entry.doc_id))
                    .cloned()
                    .collect()
            } else {
                stage_result.entries().to_vec()
            };
            entries.sort_by(|a, b| {
                b.payload
                    .score
                    .total_cmp(&a.payload.score)
                    .then_with(|| a.doc_id.cmp(&b.doc_id))
            });
            let keep = match stage.cutoff {
                MultiStageCutoff::TopK(k) => k,
                MultiStageCutoff::Ratio(r) => {
                    if !r.is_finite() || !(0.0..=1.0).contains(&r) {
                        return Err(SQLError::TypeMismatch(format!(
                            "MultiStage ratio must be finite and in [0, 1], got {r}"
                        )));
                    }
                    ((entries.len() as f64) * r).ceil() as usize
                }
            };
            entries.truncate(keep);
            entries.sort_by_key(|e| e.doc_id);
            current = Some(PostingList::from_sorted_unchecked(entries));
        }
        current.ok_or_else(|| {
            SQLError::Internal(
                "MultiStage invariant violated: non-empty stages produced no final posting list"
                    .to_string(),
            )
        })
    }

    pub(super) fn execute_progressive_fusion(
        &self,
        stages: &[uqa_operators::ProgressiveFusionEntry],
        alpha: f64,
        gating: &GatingSpec,
    ) -> DriverResult<PostingList> {
        use uqa_operators::{Operator, ProgressiveFusionOperator};

        if stages.is_empty() {
            return Err(SQLError::TypeMismatch(
                "ProgressiveFusion requires at least one stage".to_string(),
            ));
        }
        if !alpha.is_finite() || !(0.0..=1.0).contains(&alpha) {
            return Err(SQLError::TypeMismatch(format!(
                "ProgressiveFusion.alpha must be finite and in [0, 1], got {alpha}"
            )));
        }
        let mut runtime_stages = Vec::with_capacity(stages.len());
        for stage in stages {
            runtime_stages.push((
                vec![static_operator(self.execute_posting_node(&stage.signal)?)],
                stage.k,
            ));
        }
        let gating = match gating {
            GatingSpec::Softplus => "softplus",
            GatingSpec::Pass => "pass",
            GatingSpec::Sigmoid { .. } => "sigmoid",
            GatingSpec::ReLU => "relu",
            GatingSpec::Swish => "swish",
            GatingSpec::Gelu => "gelu",
        };
        let operator =
            ProgressiveFusionOperator::with_gating(runtime_stages, alpha, Some(gating.into()));
        operator
            .execute(&self.bridge_context()?)
            .map_err(|error| operator_execution_error("ProgressiveFusion", error))
    }

    pub(super) fn execute_deep_fusion(
        &self,
        layers: &[uqa_operators::DeepFusionLayer],
        alpha: f64,
        gating: &GatingSpec,
    ) -> DriverResult<PostingList> {
        use uqa_operators::Operator;

        if layers.is_empty() {
            return Err(SQLError::TypeMismatch(
                "DeepFusion requires at least one layer".to_string(),
            ));
        }
        if !matches!(
            layers.first(),
            Some(uqa_operators::DeepFusionLayer::Signal { .. })
        ) {
            return Err(SQLError::TypeMismatch(
                "DeepFusion's first layer must be Signal".to_string(),
            ));
        }
        if !alpha.is_finite() || !(0.0..=1.0).contains(&alpha) {
            return Err(SQLError::TypeMismatch(format!(
                "DeepFusion.alpha must be finite and in [0, 1], got {alpha}"
            )));
        }

        let graph_aware = layers.iter().any(|layer| {
            matches!(
                layer,
                uqa_operators::DeepFusionLayer::Propagate { .. }
                    | uqa_operators::DeepFusionLayer::Conv { .. }
                    | uqa_operators::DeepFusionLayer::Pool { .. }
            )
        });
        let mut graph_names = BTreeSet::new();
        for layer in layers {
            if let uqa_operators::DeepFusionLayer::Signal { signals } = layer {
                for signal in signals {
                    collect_graph_names(signal, &mut graph_names);
                }
            }
        }
        if graph_aware && graph_names.len() != 1 {
            return Err(SQLError::TypeMismatch(format!(
                "graph-aware DeepFusion requires exactly one graph-bearing signal, found {graph_names:?}"
            )));
        }

        let runtime_layers = layers
            .iter()
            .map(|layer| self.lower_deep_layer(layer))
            .collect::<DriverResult<Vec<_>>>()?;
        let runtime_gating = deep_runtime_gating(gating);
        let operator = uqa_ml::DeepFusionOperator::new(runtime_layers, alpha, runtime_gating)
            .map_err(|error| SQLError::TypeMismatch(error.to_string()))?;
        let mut context = self.bridge_context()?;
        if let Some(graph) = graph_names.into_iter().next() {
            let snapshot = self.with_graph(&graph, |store| {
                Ok(
                    std::sync::Arc::new(GraphNeighborSnapshot::from_store(store, &graph)?)
                        as std::sync::Arc<dyn uqa_operators::GraphNeighborLookup>,
                )
            })?;
            context.graph = Some(snapshot);
        }
        operator
            .execute(&context)
            .map_err(|error| operator_execution_error("DeepFusion", error))
    }

    pub(super) fn lower_deep_layer(
        &self,
        layer: &uqa_operators::DeepFusionLayer,
    ) -> DriverResult<uqa_ml::Layer> {
        match layer {
            uqa_operators::DeepFusionLayer::Signal { signals } => Ok(uqa_ml::Layer::Signal(
                self.execute_posting_branches(signals)?
                    .into_iter()
                    .map(static_operator)
                    .collect(),
            )),
            uqa_operators::DeepFusionLayer::Propagate {
                edge_label,
                aggregation,
                direction,
            } => Ok(uqa_ml::Layer::Propagate {
                edge_label: edge_label.clone().unwrap_or_default(),
                aggregation: match aggregation {
                    uqa_operators::DeepFusionAggregation::Mean => uqa_ml::DeepAggKind::Mean,
                    uqa_operators::DeepFusionAggregation::Sum => uqa_ml::DeepAggKind::Sum,
                    uqa_operators::DeepFusionAggregation::Max => uqa_ml::DeepAggKind::Max,
                },
                direction: *direction,
            }),
            uqa_operators::DeepFusionLayer::Conv {
                edge_label,
                hop_weights,
                direction,
            } => lower_deep_conv(edge_label.as_deref(), hop_weights, *direction),
            uqa_operators::DeepFusionLayer::Pool {
                edge_label,
                pool_size,
                method,
                direction,
            } => lower_deep_pool(edge_label.as_deref(), *pool_size, *method, *direction),
            uqa_operators::DeepFusionLayer::Flatten => Ok(uqa_ml::Layer::Flatten),
            uqa_operators::DeepFusionLayer::Dense {
                weights,
                bias,
                output_channels,
                input_channels,
            } => lower_deep_dense(weights, bias, *output_channels, *input_channels),
            uqa_operators::DeepFusionLayer::Softmax => Ok(uqa_ml::Layer::Softmax),
            uqa_operators::DeepFusionLayer::BatchNorm { epsilon } => {
                lower_deep_batch_norm(*epsilon)
            }
            uqa_operators::DeepFusionLayer::Dropout { probability } => {
                lower_deep_dropout(*probability)
            }
        }
    }
}
