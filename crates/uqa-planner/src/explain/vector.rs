//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Actual invocations remain separate from estimated physical plan nodes.

use super::properties::{object, Object, Property as P};
use uqa_core::{
    memory::{ProductionControl, ProductionString},
    vector_execution::{DiskANNExecutionRoute, VectorSearchOperation},
};
use uqa_sql::{result::vector::ExplainVectorSearch, SQLError};

pub(super) fn record<'a>(
    search: &'a ExplainVectorSearch,
    control: ProductionControl<'_>,
) -> Result<P<'a>, SQLError> {
    let work = search.work;
    let traversal = search.traversal;
    let mut database = ProductionString::new(control);
    for byte in search.generation.database {
        database.push_str(&control.format(format_args!("{byte:02x}"))?)?;
    }
    let generation = search.generation;
    let mut value = Object::new(control);
    for (key, property) in [
        ("Relation", P::Text(&search.relation)),
        ("Field", P::Text(&search.field)),
        ("Returned Documents", P::Unsigned(search.returned_documents)),
        ("Index Type", P::Text("diskann")),
        (
            "Generation",
            object(
                control,
                [
                    ("Database", P::OwnedText(database.finish()?)),
                    ("Table", P::Unsigned(generation.table)),
                    ("Index", P::Unsigned(generation.index)),
                    ("Generation", P::Unsigned(generation.generation)),
                ],
            )?,
        ),
        (
            "Route",
            P::Text(match search.route {
                DiskANNExecutionRoute::Approximate => "approximate",
                DiskANNExecutionRoute::ExactZeroNorm => "exact zero norm",
                DiskANNExecutionRoute::ExactNonFiniteNorm => "exact non-finite norm",
                DiskANNExecutionRoute::ExactThreshold => "exact threshold",
                DiskANNExecutionRoute::EmptyK => "empty k",
            }),
        ),
        (
            "Traversal",
            object(
                control,
                [
                    (
                        "Approximate Expansions",
                        P::Unsigned(traversal.approximate_expansions),
                    ),
                    (
                        "Completion Expansions",
                        P::Unsigned(traversal.completion_expansions),
                    ),
                    ("PQ Estimates", P::Unsigned(traversal.pq_estimates)),
                    ("Beams", P::Unsigned(traversal.beams)),
                ],
            )?,
        ),
        (
            "Logical Page Reads",
            object(
                control,
                [
                    ("Requests", P::Unsigned(work.pages.page_requests)),
                    ("Cache Hits", P::Unsigned(work.pages.cache_hits)),
                    ("Provider Pages", P::Unsigned(work.pages.provider_pages)),
                    ("Provider Batches", P::Unsigned(work.pages.provider_batches)),
                ],
            )?,
        ),
        ("Side Entries", P::Unsigned(work.side_entries)),
        (
            "Scoring",
            object(
                control,
                [
                    ("Reranked", scoring(work.reranked, control)?),
                    ("Changed", scoring(work.changed, control)?),
                    ("Unversioned", scoring(work.unversioned, control)?),
                    ("Exact", scoring(work.exact, control)?),
                ],
            )?,
        ),
    ] {
        value.insert(key, property)?;
    }
    match search.operation {
        VectorSearchOperation::KNN { k } => {
            value.insert("Operation", P::Text("knn"))?;
            value.insert("Requested K", P::Unsigned(k as u64))?;
        }
        VectorSearchOperation::Threshold { threshold } => {
            value.insert("Operation", P::Text("threshold"))?;
            value.insert("Threshold", P::Float(threshold.into()))?;
        }
    }
    value.finish()
}

fn scoring(
    stats: uqa_core::vector_execution::DiskANNScoringStats,
    control: ProductionControl<'_>,
) -> Result<P<'static>, SQLError> {
    object(
        control,
        [
            ("Documents", P::Unsigned(stats.documents)),
            ("Vectors", P::Unsigned(stats.vectors)),
        ],
    )
}

pub(super) fn append_text(
    text: &mut ProductionString<'_>,
    analysis: &uqa_sql::result::ExplainAnalysis,
    control: ProductionControl<'_>,
) -> Result<(), SQLError> {
    for search in analysis.vector_searches.iter() {
        let rendered = super::formats::json(&record(search, control)?, control)?;
        text.push_str("\n  Vector Search:")?;
        for line in rendered.lines() {
            text.push_str("\n    ")?;
            text.push_str(line)?;
        }
    }
    Ok(())
}
