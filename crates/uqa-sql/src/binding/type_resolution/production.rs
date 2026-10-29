//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected expression types retain their producer owner through ordered preparation.

use super::{
    ColumnType, QueryPlan, RoutineResolution, RowSchema, SQLError, SQLParam, ScalarExpr,
    SchemaScope,
};
use uqa_core::memory::Produced;

impl SchemaScope {
    pub(in crate::binding) fn bind_expression_type_with_control(
        &mut self,
        routines: &dyn RoutineResolution,
        expression: &ScalarExpr,
        schema: &RowSchema,
        subqueries: &[QueryPlan],
        params: &[SQLParam],
    ) -> Result<Option<Produced<ColumnType>>, SQLError> {
        let owner = self.control().capture();
        let control = owner.control();
        control.check()?;
        if let ScalarExpr::ScalarSubquery(index) = expression {
            let plan = subqueries.get(*index).ok_or_else(|| {
                SQLError::Internal(format!("scalar subquery slot {index} is out of bounds"))
            })?;
            let output = self.bind_query(routines, plan, params, Some(schema))?;
            return output
                .column_type(0)
                .map(|ty| ty.clone_with_control(&control).map_err(Into::into))
                .transpose();
        }
        let schema = self.with_stored_outer_internal_aliases(schema)?;
        let resolver =
            self.query_function_type_resolver(routines, expression, &schema, subqueries, params)?;
        if self.validate_references {
            Self::validate_expression_references_with_resolver(
                self.rule_condition_event,
                routines,
                expression,
                &schema,
                None,
                params,
                &resolver,
            )?;
        }
        crate::type_resolution::validate_catalog_literals_with_control(
            expression, &schema, params, &resolver, &control,
        )?;
        crate::type_resolution::scalar_type_with_resolver_and_control(
            expression,
            &schema,
            params,
            Some(&resolver),
            &control,
        )
    }
}
