//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar evaluation and physical retrieval construction at the execution boundary.

use super::instantiate::instantiate;
use crate::{eval_scalar, ScalarEvalContext};
use uqa_core::Value;
use uqa_operators::OperatorTree;
use uqa_sql::{
    ast::OperatorJoinRelations,
    expr::EngineHook,
    retrieval::{self, RetrievalArguments, RetrievalConstants},
    semantics::graph_functions::GraphNameCatalog,
    SQLError, SQLParam, ScalarExpr,
};

fn evaluate_constant(expression: &ScalarExpr, params: &[SQLParam]) -> Result<Value, SQLError> {
    eval_scalar(expression, &ScalarEvalContext::new(None, params))
}

/// Preserve the public syntax-lowering API while constructing its physical models in execution.
pub fn lower_where(expression: &ScalarExpr, params: &[SQLParam]) -> Option<OperatorTree> {
    retrieval::lower_where(
        expression,
        &RetrievalConstants {
            params,
            evaluate: &evaluate_constant,
        },
    )
    .and_then(|logical| instantiate(logical).ok())
}

pub struct RetrievalBinding<'a> {
    pub hook: &'a dyn EngineHook,
    pub graphs: &'a dyn GraphNameCatalog,
}
impl GraphNameCatalog for RetrievalBinding<'_> {
    fn list_graphs(&self) -> Result<Vec<String>, SQLError> {
        self.graphs.list_graphs()
    }
}
impl RetrievalArguments for RetrievalBinding<'_> {
    fn evaluate_argument(
        &self,
        expression: &ScalarExpr,
        params: &[SQLParam],
    ) -> Result<Value, SQLError> {
        eval_scalar(
            expression,
            &ScalarEvalContext::new(None, params).with_function_hook(self.hook),
        )
    }
}
impl RetrievalBinding<'_> {
    pub fn lower_where(
        &self,
        expression: &ScalarExpr,
        params: &[SQLParam],
    ) -> Result<Option<OperatorTree>, SQLError> {
        retrieval::lower_where_bound(
            self,
            expression,
            &RetrievalConstants {
                params,
                evaluate: &evaluate_constant,
            },
        )?
        .map(instantiate)
        .transpose()
    }
    pub fn lower_function(
        &self,
        name: &str,
        args: &[ScalarExpr],
        params: &[SQLParam],
    ) -> Result<OperatorTree, SQLError> {
        instantiate(retrieval::lower_sql_function_bound(
            self,
            name,
            args,
            &RetrievalConstants {
                params,
                evaluate: &evaluate_constant,
            },
        )?)
    }
    pub fn lower_join(
        &self,
        name: &str,
        relations: Option<&OperatorJoinRelations>,
        args: &[ScalarExpr],
        params: &[SQLParam],
    ) -> Result<(OperatorJoinRelations, OperatorTree), SQLError> {
        let (relations, logical) = retrieval::lower_operator_join_table_function(
            self,
            name,
            relations,
            args,
            &RetrievalConstants {
                params,
                evaluate: &evaluate_constant,
            },
        )?;
        Ok((relations, instantiate(logical)?))
    }
}

impl RetrievalBinding<'_> {
    /// Describe a predicate that owns one bounded vector candidate pool across a relation hierarchy.
    pub fn direct_vector_retrieval(
        &self,
        expression: &ScalarExpr,
        params: &[SQLParam],
    ) -> Result<Option<crate::query::table_sources::retrieval::DirectVectorRetrieval>, SQLError>
    {
        use crate::query::table_sources::retrieval::DirectVectorRetrieval;
        let Some(tree) = self.lower_where(expression, params)? else {
            return Ok(None);
        };
        Ok(match tree {
            OperatorTree::KNN { k, .. } => Some(DirectVectorRetrieval::Knn { top_k: k }),
            OperatorTree::CalibratedVectorMatch {
                field,
                query_vector,
                k,
                threshold,
            } => Some(DirectVectorRetrieval::Calibrated {
                field,
                query_vector,
                top_k: k,
                threshold,
            }),
            _ => None,
        })
    }
}

#[cfg(test)]
mod tests;
