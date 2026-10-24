//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Successful subquery bindings within one clause-validation invocation.

use std::cell::RefCell;
use std::collections::BTreeSet;

use super::{
    QueryFunctionTypeResolver, QueryPlan, RoutineResolution, RowSchema, SQLError, SQLParam,
    ScalarExpr, SchemaScope,
};

struct QueryResult {
    input: RowSchema,
    output: RowSchema,
}

/// The caller retains one analysis scope for these clauses. Parameters, routines and the borrowed query arena cannot change, and every hit additionally requires the complete lexical input schema. Results are discarded when that invocation ends; errors are never retained.
pub(in crate::binding) struct ValidatedSubqueries<'r, 'a> {
    pub(in crate::binding) routines: &'r dyn RoutineResolution,
    pub(in crate::binding) plans: &'a [QueryPlan],
    pub(in crate::binding) params: &'a [SQLParam],
    results: Option<RefCell<Vec<Option<QueryResult>>>>,
}

impl<'r, 'a> ValidatedSubqueries<'r, 'a> {
    pub(in crate::binding) fn new(
        routines: &'r dyn RoutineResolution,
        plans: &'a [QueryPlan],
        params: &'a [SQLParam],
    ) -> Self {
        Self {
            routines,
            plans,
            params,
            results: None,
        }
    }

    pub(in crate::binding) fn retaining(mut self) -> Self {
        self.results = Some(RefCell::new(Vec::new()));
        self
    }

    pub(in crate::binding) fn resolver(
        &self,
        scope: &mut SchemaScope,
        expressions: &[ScalarExpr],
        input: &RowSchema,
    ) -> Result<QueryFunctionTypeResolver<'r>, SQLError> {
        let mut referenced = BTreeSet::new();
        for expression in expressions {
            crate::semantics::collect_subquery_ids(expression, &mut referenced);
        }
        let mut schemas = (!referenced.is_empty()).then(|| vec![None; self.plans.len()]);
        for index in referenced {
            let plan = self.plans.get(index).ok_or_else(|| {
                SQLError::Internal(format!("scalar subquery slot {index} is out of bounds"))
            })?;
            let previous = self.results.as_ref().and_then(|results| {
                results
                    .borrow()
                    .get(index)
                    .and_then(Option::as_ref)
                    .filter(|result| result.input == *input)
                    .map(|result| result.output.clone())
            });
            let output = if let Some(output) = previous {
                output
            } else {
                let output = scope.bind_query(self.routines, plan, self.params, Some(input))?;
                if let Some(results) = &self.results {
                    let mut results = results.borrow_mut();
                    results.resize_with(self.plans.len(), || None);
                    results[index] = Some(QueryResult {
                        input: input.clone(),
                        output: output.clone(),
                    });
                }
                output
            };
            schemas
                .as_mut()
                .expect("referenced queries allocate outputs")[index] = Some(output);
        }
        Ok(QueryFunctionTypeResolver {
            routines: self.routines,
            scalar_subquery_schemas: schemas,
            defer_routine_namespace_errors: false,
        })
    }
}
