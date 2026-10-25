//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Project retained vector facts and explicitly separate estimates from observations.

use super::{
    metadata::{index_node, work_node},
    Collector,
};
use crate::cost_model::{estimate_diskann, DiskANNWorkEstimate};
use crate::retrieval_planning::VectorStatisticsRead;
use uqa_core::{DiskANNIndexStats, VectorQueryRoute};
use uqa_sql::{
    ast::FunctionVolatility,
    retrieval::{describe_vector_call_with_control, ProducedVectorCallDescription, RetrievalExpr},
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

impl Collector<'_, '_, '_> {
    pub(super) fn vector_predicate(
        &mut self,
        table: &str,
        qualifier: &str,
        predicate: &ScalarExpr,
        path: &str,
    ) -> Result<(), SQLError> {
        let params = self.params;
        let evaluate = self.context.evaluate;
        let volatility = self.context.filters.volatility;
        let parameters = self.params.len();
        let mut retained = None;
        predicate.try_visit(&mut |expression| -> Result<bool, SQLError> {
            self.control.check()?;
            if let Some(description) = describe_vector_call_with_control(
                expression,
                params,
                evaluate,
                &|expr| can_evaluate(volatility, expr, parameters),
                &self.control,
            )? {
                if description.invalid_arguments || description.field.is_none() {
                    return Ok(false);
                }
                if retained.is_none() {
                    retained = Some(self.context.retrieval.try_query_table(table).map_err(
                        |error| {
                            SQLError::Internal(format!("read EXPLAIN table `{table}`: {error}"))
                        },
                    )?);
                }
                if let Some(table_metadata) = retained.as_ref().and_then(Option::as_ref) {
                    self.capture_vector(
                        &*table_metadata.vector_indexes(),
                        table,
                        qualifier,
                        path,
                        &description,
                    )?;
                }
                return Ok(false);
            }
            Ok(match expression {
                ScalarExpr::And(_) | ScalarExpr::Or(_) | ScalarExpr::Not(_) => true,
                ScalarExpr::Func { name, .. } => uqa_sql::registry::is_registered(name),
                _ => false,
            })
        })?;
        Ok(())
    }

    fn capture_vector(
        &mut self,
        reader: &dyn VectorStatisticsRead,
        table: &str,
        qualifier: &str,
        path: &str,
        description: &ProducedVectorCallDescription,
    ) -> Result<(), SQLError> {
        let field = description.field.as_deref().expect("selected vector field");
        let bound = vector_arguments(description);
        let Some(PhysicalVector {
            index,
            route,
            estimate,
        }) = physical_vector(reader, field, bound, self.costs, &|| {
            self.context.retrieval.table_doc_count(table)
        })?
        else {
            return Ok(());
        };
        self.control.check()?;
        let mut node = index_node(&index, self.control)?;
        node.label("Node Type", "DiskANN Search")?;
        node.text("Plan Path", path)?;
        node.text("Relation Name", table)?;
        node.text("Alias", qualifier)?;
        node.text("Field", field)?;
        node.optional_number("Candidate K", bound.map(|(_, k)| k as u64))?;
        node.label(
            "Argument Status",
            if route.is_some() { "bound" } else { "deferred" },
        )?;
        if let Some(route) = route {
            node.label(
                "Route",
                match route {
                    VectorQueryRoute::Approximate => "approximate",
                    VectorQueryRoute::ExactZeroNorm => "exact zero norm",
                    VectorQueryRoute::ExactNonFiniteNorm => "exact non-finite norm",
                },
            )?;
        } else {
            node.null("Route")?;
        }
        node.label(
            "Score",
            if description.calibrated {
                "query-pool calibrated cosine"
            } else {
                "raw cosine"
            },
        )?;
        node.boolean("Candidate Refill After Residual Filter", false)?;
        if self.costs {
            if let Some(estimate) = estimate {
                node.insert("Estimated Work", work_node(estimate, self.control)?)?;
            } else {
                node.null("Estimated Work")?;
            }
        }
        self.output.push_produced(
            self.control
                .finish(node.finish()?, self.control.empty_reservation())?,
        )?;
        Ok(())
    }
}

fn physical_vector(
    reader: &dyn VectorStatisticsRead,
    field: &str,
    query: Option<(&[f32], usize)>,
    costs: bool,
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
        let estimate = costs
            .then(|| documents().map(|documents| estimate_diskann(&stats, Some(k), documents)))
            .transpose()?;
        Ok(Some(PhysicalVector {
            index: stats.index,
            route: Some(stats.query_route),
            estimate,
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

fn vector_arguments(description: &ProducedVectorCallDescription) -> Option<(&[f32], usize)> {
    match &**description.bound.as_ref()? {
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
