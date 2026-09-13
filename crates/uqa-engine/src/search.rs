//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Text, vector, and hybrid retrieval orchestration.

use std::collections::BTreeMap;
use std::time::Instant;

use super::{
    Arc, BM25Params, BayesianBM25Params, BayesianBM25Scorer, CalibrationMetrics, CalibrationReport,
    DocId, Engine, ExecutionContext, HybridSearchParams, ParameterLearner, RawBm25Score,
    RobustHybridSearchParams, SQLError, ScoredEntry, ScoringMode, StorageBackendError,
    TextSearchAlgorithm, TextSearchProfile, UnsupervisedBm25ScoreEstimator,
};
use uqa_operators::{OperatorTree, TextScoringMode, TextTopKPlan, TextTopKStrategy};
use uqa_storage::{inverted_index::analyze_query_terms, TokenTermKey};

mod calibration;
mod context;
mod helpers;
mod hybrid;
mod learning;
mod search_api;
mod top_k;
mod vector;

pub(crate) use helpers::storage_sql_error;
