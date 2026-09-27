//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Actual invocations remain separate from estimated physical plan nodes.

use std::fmt::Write as _;
use uqa_core::vector_execution::{DiskANNExecutionRoute, VectorSearchOperation};
use uqa_sql::{result::vector::ExplainVectorSearch, SQLError};

pub(super) fn record(search: &ExplainVectorSearch) -> serde_json::Value {
    let work = search.work;
    let traversal = search.traversal;
    let mut value = serde_json::json!({
        "Relation": &*search.relation,
        "Field": &*search.field,
        "Returned Documents": search.returned_documents,
        "Index Type": "diskann",
        "Generation": super::render_generation(search.generation),
        "Route": match search.route {
            DiskANNExecutionRoute::Approximate => "approximate",
            DiskANNExecutionRoute::ExactZeroNorm => "exact zero norm",
            DiskANNExecutionRoute::ExactNonFiniteNorm => "exact non-finite norm",
            DiskANNExecutionRoute::ExactThreshold => "exact threshold",
            DiskANNExecutionRoute::EmptyK => "empty k",
        },
        "Traversal": {
            "Approximate Expansions": traversal.approximate_expansions,
            "Completion Expansions": traversal.completion_expansions,
            "PQ Estimates": traversal.pq_estimates,
            "Beams": traversal.beams,
        },
        "Logical Page Reads": {
            "Requests": work.pages.page_requests,
            "Cache Hits": work.pages.cache_hits,
            "Provider Pages": work.pages.provider_pages,
            "Provider Batches": work.pages.provider_batches,
        },
        "Side Entries": work.side_entries,
        "Scoring": {
            "Reranked": scoring(work.reranked),
            "Changed": scoring(work.changed),
            "Unversioned": scoring(work.unversioned),
            "Exact": scoring(work.exact),
        },
    });
    match search.operation {
        VectorSearchOperation::KNN { k } => {
            value["Operation"] = "knn".into();
            value["Requested K"] = k.into();
        }
        VectorSearchOperation::Threshold { threshold } => {
            value["Operation"] = "threshold".into();
            value["Threshold"] = threshold.into();
        }
    }
    value
}

fn scoring(stats: uqa_core::vector_execution::DiskANNScoringStats) -> serde_json::Value {
    serde_json::json!({"Documents": stats.documents, "Vectors": stats.vectors})
}

pub(super) fn append_text(
    text: &mut String,
    analysis: &uqa_sql::result::ExplainAnalysis,
) -> Result<(), SQLError> {
    for search in analysis.vector_searches.iter() {
        let rendered = serde_json::to_string_pretty(&record(search))
            .map_err(|error| SQLError::Internal(format!("format vector execution: {error}")))?;
        text.push_str("\n  Vector Search:");
        for line in rendered.lines() {
            let _ = write!(text, "\n    {line}");
        }
    }
    Ok(())
}
