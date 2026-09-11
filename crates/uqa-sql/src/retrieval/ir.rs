//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bound retrieval expressions contain values and configuration; execution models are constructed by the executor.

use uqa_core::{
    retrieval::{Direction, ExternalPriorMode, GatingSpec, MultiStageCutoff, TemporalFilterIR},
    Predicate,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextScoringMode {
    BM25,
    BayesianBM25,
}

#[derive(Clone, Debug)]
pub enum AttentionSpec {
    Single {
        alpha: f64,
        normalized: bool,
        base_rate: Option<f64>,
    },
    MultiHead {
        n_heads: usize,
        alpha: f64,
        normalized: bool,
    },
}

#[derive(Clone, Debug)]
pub struct MultiStageEntry {
    pub child: RetrievalExpr,
    pub cutoff: MultiStageCutoff,
}

/// SQL-owned retrieval algebra. Physical scorers, fusers, closures, index choices and top-k strategies do not belong to this representation.
#[derive(Clone, Debug)]
pub enum RetrievalExpr {
    Empty,
    Term {
        query: String,
        field: Option<String>,
        scoring: Option<TextScoringMode>,
    },
    Filter {
        field: String,
        predicate: Predicate,
        source: Option<Box<Self>>,
    },
    BayesianScore {
        source: Box<Self>,
        field: Option<String>,
    },
    BayesianMatchWithPrior {
        field: String,
        query: String,
        prior_field: String,
        mode: ExternalPriorMode,
    },
    Intersect(Vec<Self>),
    Union(Vec<Self>),
    Complement(Box<Self>),
    Composed(Vec<Self>),
    EncodeGraphPosting {
        source: Box<Self>,
    },
    KNN {
        query_vector: Vec<f32>,
        k: usize,
        field: String,
    },
    CalibratedVectorMatch {
        query_vector: Vec<f32>,
        k: usize,
        field: String,
        threshold: Option<f64>,
    },
    CosineProbability(Box<Self>),
    BayesianEvidenceFusion {
        signals: Vec<Self>,
        base_rate: Option<f64>,
    },
    RobustPositiveEvidencePool {
        signals: Vec<Self>,
        alpha: f64,
        gating: GatingSpec,
        weights: Option<Vec<f64>>,
        logit_min: Option<Vec<f64>>,
        logit_max: Option<Vec<f64>>,
        adaptive_weights: bool,
    },
    AttentionFusion {
        signals: Vec<Self>,
        options: AttentionSpec,
        function_name: String,
    },
    LearnedFusion {
        signals: Vec<Self>,
        alpha: f64,
    },
    SparseThreshold {
        source: Box<Self>,
        threshold: f64,
    },
    Traverse {
        start_vertex: u64,
        graph: String,
        label: Option<String>,
        max_hops: usize,
    },
    GraphNeighbors {
        vertex: u64,
        graph: String,
        label: Option<String>,
        direction: Direction,
    },
    GraphEdges {
        graph: String,
        label: Option<String>,
    },
    RegularPathQuery {
        rpq_source: String,
        start_vertex: u64,
        graph: String,
    },
    TemporalTraverse {
        start_vertex: u64,
        graph: String,
        label: Option<String>,
        max_hops: usize,
        temporal_filter: Option<TemporalFilterIR>,
    },
    PageRank {
        graph: String,
    },
    HITS {
        graph: String,
    },
    BetweennessCentrality {
        graph: String,
    },
    DeepPredict {
        model: String,
    },
    MultiStage {
        stages: Vec<MultiStageEntry>,
    },
    MultiFieldSearch {
        fields: Vec<String>,
        queries: Vec<String>,
        weights: Option<Vec<f64>>,
    },
    TextSimilarityJoin {
        left: Box<Self>,
        right: Box<Self>,
        threshold: f64,
    },
    VectorSimilarityJoin {
        left: Box<Self>,
        right: Box<Self>,
        threshold: f64,
    },
    GraphJoin {
        left: Box<Self>,
        right: Box<Self>,
        label: Option<String>,
        graph: String,
    },
    HybridJoin {
        left: Box<Self>,
        right: Box<Self>,
    },
    CrossParadigmJoin {
        left: Box<Self>,
        right: Box<Self>,
    },
}
