//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Static row types and validation for recursive CTE control columns.

use super::{BindingContext, QueryPlan, RowSchema, SQLError, SQLParam, SchemaScope};
use crate::routines::RoutineResolution;
use uqa_core::memory::ProductionControl;
mod production;
pub use production::{
    extend_cte_generated_schema_with_control, extend_recursive_cte_binding_schema_with_control,
};

pub fn hide_recursive_generated_schema(schema: &RowSchema, visible: usize) -> RowSchema {
    RowSchema::with_recursive_input_with_control(
        schema,
        visible,
        &ProductionControl::uncontrolled(),
    )
    .expect("ordinary recursive input schema")
}

pub fn extend_cte_generated_schema(
    routines: &dyn RoutineResolution,
    cte: &crate::plan::CtePlan,
    schema: RowSchema,
    params: &[SQLParam],
) -> Result<RowSchema, SQLError> {
    extend_cte_generated_schema_with_control(
        routines,
        cte,
        schema,
        params,
        &ProductionControl::uncontrolled(),
    )
}

pub fn extend_recursive_cte_binding_schema(
    routines: &dyn RoutineResolution,
    cte: &crate::plan::CtePlan,
    schema: RowSchema,
    params: &[SQLParam],
) -> Result<RowSchema, SQLError> {
    extend_recursive_cte_binding_schema_with_control(
        routines,
        cte,
        schema,
        params,
        &ProductionControl::uncontrolled(),
    )
}

pub fn analyze_recursive_control_step(
    routines: &dyn RoutineResolution,
    cte: &crate::plan::CtePlan,
    step: &QueryPlan,
    base_schema: RowSchema,
    params: &[SQLParam],
    ctes: &BindingContext,
) -> Result<(), SQLError> {
    let provisional = extend_recursive_cte_binding_schema(routines, cte, base_schema, params)?;
    let mut scope = SchemaScope::for_analysis(ctes)?;
    scope.ctes.insert(cte.name.clone(), provisional);
    scope.bind_query(routines, step, params, None).map(|_| ())
}
