//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Project retained vector facts and explicitly separate estimates from observations.

use super::Collector;
use crate::cost_model::{estimate_diskann, DiskANNWorkEstimate};
use crate::retrieval_planning::VectorStatisticsRead;
use serde_json::{json, Value as Json};
use uqa_core::{DiskANNIndexStats, VectorQueryRoute};
use uqa_sql::{
    ast::FunctionVolatility,
    retrieval::{describe_vector_call, RetrievalConstants, RetrievalExpr, VectorCallDescription},
    semantics::volatility::{function_volatility_with_binding, VolatilityCatalog},
    SQLError, ScalarExpr,
};

#[cfg(test)]
mod tests;

struct PhysicalVector {
    index: DiskANNIndexStats,
    route: Option<VectorQueryRoute>,
    estimate: Option<DiskANNWorkEstimate>,
}

impl Collector<'_> {
    pub(super) fn vector_predicate(
        &mut self,
        table: &str,
        qualifier: &str,
        predicate: &ScalarExpr,
        path: &str,
    ) -> Result<(), SQLError> {
        let mut descriptions = Vec::new();
        let constants = RetrievalConstants {
            params: self.params,
            evaluate: self.context.evaluate,
            stores: &|_: &str| false,
        };
        predicate.try_visit(&mut |expression| -> Result<bool, SQLError> {
            if let Some(description) = describe_vector_call(expression, &constants, &|expr| {
                can_evaluate(self.context.filters.volatility, expr, self.params.len())
            })? {
                if !description.invalid_arguments && description.field.is_some() {
                    descriptions.push(description);
                }
                return Ok(false);
            }
            Ok(match expression {
                ScalarExpr::And(_) | ScalarExpr::Or(_) | ScalarExpr::Not(_) => true,
                ScalarExpr::Func { name, .. } => uqa_sql::registry::is_registered(name),
                _ => false,
            })
        })?;
        if descriptions.is_empty() {
            return Ok(());
        }
        let Some(retained) = self
            .context
            .retrieval
            .try_query_table(table)
            .map_err(|error| {
                SQLError::Internal(format!("read EXPLAIN table `{table}`: {error}"))
            })?
        else {
            return Ok(());
        };
        let reader = retained.vector_indexes();
        for description in descriptions {
            let Some(field) = description.field.as_deref() else {
                continue;
            };
            let bound = vector_arguments(&description);
            let Some(PhysicalVector {
                index,
                route,
                estimate,
            }) = physical_vector(&*reader, field, bound, &|| {
                self.context.retrieval.table_doc_count(table)
            })?
            else {
                continue;
            };
            let mut node = index_node(&index);
            node["Node Type"] = json!("DiskANN Search");
            node["Plan Path"] = json!(path);
            node["Relation Name"] = json!(table);
            node["Alias"] = json!(qualifier);
            node["Field"] = json!(field);
            node["Candidate K"] = json!(bound.map(|(_, k)| k));
            node["Argument Status"] = json!(if route.is_some() { "bound" } else { "deferred" });
            node["Route"] = json!(route.map(|route| match route {
                VectorQueryRoute::Approximate => "approximate",
                VectorQueryRoute::ExactZeroNorm => "exact zero norm",
                VectorQueryRoute::ExactNonFiniteNorm => "exact non-finite norm",
            }));
            node["Score"] = json!(if description.calibrated {
                "query-pool calibrated cosine"
            } else {
                "raw cosine"
            });
            node["Candidate Refill After Residual Filter"] = json!(false);
            node["Estimated Work"] = estimate.map_or(Json::Null, work_node);
            self.output.nodes.push(node);
        }
        Ok(())
    }
}

fn physical_vector(
    reader: &dyn VectorStatisticsRead,
    field: &str,
    query: Option<(&[f32], usize)>,
    documents: &dyn Fn() -> Result<u64, SQLError>,
) -> Result<Option<PhysicalVector>, SQLError> {
    if let Some((query, k)) = query {
        // Preserve ordinary costing's boundary: reached execution owns invalid
        // argument diagnostics before any physical metadata read can fail.
        if query.is_empty()
            || query.iter().any(|value| !value.is_finite())
            || reader
                .dimensions(field)
                .is_none_or(|dimensions| dimensions as usize != query.len())
        {
            return Ok(None);
        }
        let Some(stats) = reader.diskann_query_statistics(field, query)? else {
            return Ok(None);
        };
        let estimate = estimate_diskann(&stats, Some(k), documents()?);
        Ok(Some(PhysicalVector {
            index: stats.index,
            route: Some(stats.query_route),
            estimate: Some(estimate),
        }))
    } else {
        Ok(reader
            .diskann_index_statistics(field)?
            .map(|index| PhysicalVector {
                index,
                route: None,
                estimate: None,
            }))
    }
}

fn vector_arguments(description: &VectorCallDescription) -> Option<(&[f32], usize)> {
    match description.bound.as_ref()? {
        RetrievalExpr::KNN {
            query_vector, k, ..
        }
        | RetrievalExpr::CalibratedVectorMatch {
            query_vector, k, ..
        } => Some((query_vector, *k)),
        _ => None,
    }
}

fn can_evaluate(
    catalog: &dyn VolatilityCatalog,
    expression: &ScalarExpr,
    parameters: usize,
) -> bool {
    let mut safe = !expression.contains_subquery();
    expression.visit(&mut |node| match node {
        ScalarExpr::Param(index) => safe &= *index > 0 && *index <= parameters,
        ScalarExpr::Column(_)
        | ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::Position(_)
        | ScalarExpr::InternalColumn(_)
        | ScalarExpr::WindowCall { .. } => safe = false,
        ScalarExpr::Func {
            name,
            binding,
            args,
            ..
        } => {
            // This evaluator has no runtime callback binding. Never replace a
            // shadowing routine with the builtin that happens to share its name.
            let identity = name.to_ascii_lowercase();
            safe &= catalog.host_function_volatility(&identity).is_none()
                && catalog
                    .routine_volatilities(&identity, binding.as_ref())
                    .is_none();
            safe &= function_volatility_with_binding(catalog, name, binding.as_ref(), args.len())
                == FunctionVolatility::Immutable;
        }
        _ => {}
    });
    safe
}

fn index_node(stats: &DiskANNIndexStats) -> Json {
    let population = stats.populations;
    let reads = stats.reads;
    json!({
        "Generation": super::super::render_generation(stats.generation),
        "Configuration": {
            "Dimensions": stats.dimensions,
            "Maximum Degree": stats.max_degree,
            "Search List Size": stats.search_list_size,
            "Logical Beam Width": stats.beam_width,
            "PQ Bytes": stats.pq_bytes,
            "PQ Centroids": stats.pq_centroids,
        },
        "Layout": {
            "Node Slot Bytes": stats.node_slot_bytes,
            "Fragments Per Node": stats.node_fragments,
            "Graph Pages": stats.graph_pages,
            "Graph Edges": stats.graph_edges,
            "Page Bytes": stats.page_bytes,
        },
        "Population": {
            "Base Documents": population.base_documents,
            "Base Vectors": population.base_vectors,
            "Graph Nodes": population.graph_nodes,
            "Side Vectors": population.side_vectors,
            "Current Vectors": population.current_vectors,
            "Changed Vectors": population.changed_vectors,
        },
        "Read Limits": {
            "Resident Bytes": reads.resident_bytes,
            "Cache Bytes": reads.cache_bytes,
            "Maximum Record Bytes": reads.max_record_bytes,
            "Maximum In-Flight Page Bytes": reads.max_in_flight_page_bytes,
            "Maximum Batch Pages": reads.max_batch_pages,
            "Read Concurrency": reads.read_concurrency,
        },
    })
}

fn work_node(work: DiskANNWorkEstimate) -> Json {
    json!({
        "Units": "uncalibrated work; logical page requests, not measured physical I/O",
        "Approximate Nodes": work.approximate_nodes,
        "Completion Nodes": work.completion_nodes,
        "PQ Lookup Coordinates": work.pq_lookup_coordinates,
        "PQ Distance Evaluations": work.pq_distance_evaluations,
        "Logical Page Requests": work.logical_page_requests,
        "Logical Page Bytes": work.logical_page_bytes,
        "Provider Dispatch Rounds": work.page_rounds,
        "Side Vectors": work.side_vectors,
        "Changed Vectors": work.changed_vectors,
        "Rerank Vectors": work.rerank_vectors,
        "Exact Vectors": work.exact_vectors,
        "Resident PQ Payload Bytes": work.resident_pq_payload_bytes,
        "Page Budget Sufficient": work.page_budget_sufficient,
        "CPU": work.cpu,
        "I/O": work.io,
        "Total": work.total(),
    })
}
