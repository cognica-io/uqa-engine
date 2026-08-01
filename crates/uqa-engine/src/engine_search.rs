//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Text, vector, and hybrid retrieval orchestration.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use super::{
    Arc, BM25Params, BM25Scorer, BayesianBM25Params, BayesianBM25Scorer, CalibrationMetrics,
    CalibrationReport, DocId, Engine, ExecutionContext, HybridSearchParams, InvertedIndex,
    ParameterLearner, PostingList, RawBm25Score, SQLError, ScoredEntry, Scorer, ScoringMode,
    StorageBackendError, StorageBackendResult, TextSearchAlgorithm, TextSearchProfile,
    UnsupervisedBm25ScoreEstimator,
};
use uqa_core::IndexStats;
use uqa_operators::{OperatorTree, TextScoringMode, TextTopKPlan, TextTopKStrategy};
use uqa_scoring::{BlockMaxWANDScorer, WANDQuery, WANDScorer, WANDStats};
use uqa_storage::{BlockMaxIndex, DEFAULT_BLOCK_SIZE};

mod calibration;
mod context;
mod helpers;
mod hybrid;
mod learning;
mod search_api;
mod text_scoring;
mod top_k;
mod vector;

use helpers::{
    block_max_scorer_fingerprint, raw_bm25_params, search_stats_for_terms, storage_sql_error,
};
