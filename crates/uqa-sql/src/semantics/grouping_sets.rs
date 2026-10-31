//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! PostgreSQL-compatible grouping-set preparation after input schema binding.

use crate::ast::ColumnType;
use crate::{RowSchema, ScalarExpr};
#[cfg(test)]
use uqa_core::Value;

use crate::{plan::QueryBlockPlan, FunctionTypeResolver, SQLError, SQLParam};

mod expressions;
mod normalization;
mod production;
mod serialization;
pub(crate) use production::{
    analyze_distinct_grouping_with_control, expression_identity_with_control,
};
mod names;
pub(crate) use names::resolve_grouping_expression_reference_with_control;
pub use names::{
    bind_grouping_names, bind_grouping_names_owned, grouping_output_name,
    resolve_grouping_expression,
};
mod validation;
pub use validation::validate_grouped_expressions;
pub(crate) use validation::validate_grouped_expressions_with_inputs;
#[cfg(test)]
mod tests;

/// Resolve grouping names against the input before expanding output aliases and deduplicating `GROUP BY DISTINCT` sets. Equivalent qualified and unqualified columns share one identity without conflating expressions that `PostgreSQL` resolves to different operator inputs.
pub fn prepare_grouping_sets(
    engine: &dyn crate::routines::RoutineResolution,
    statement: &QueryBlockPlan,
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<Option<QueryBlockPlan>, SQLError> {
    if statement.group_by.is_empty() && statement.grouping_sets.is_empty() {
        return Ok(None);
    }

    let mut prepared = statement.clone();
    let changed = prepare_grouping_sets_in_place(engine, &mut prepared, schema, None, params)?;
    Ok(changed.then_some(prepared))
}

/// Finalize analyzed grouping identity before scalar optimization can erase it.
pub(crate) fn prepare_grouping_sets_in_place(
    engine: &dyn crate::routines::RoutineResolution,
    prepared: &mut QueryBlockPlan,
    schema: &RowSchema,
    outer: Option<&RowSchema>,
    params: &[SQLParam],
) -> Result<bool, SQLError> {
    let mut changed = bind_grouping_names(engine, prepared, schema, outer, params)?;
    changed |= expressions::bind_grouping_expressions(engine, prepared, schema, params)?;
    if !prepared.group_distinct {
        return Ok(changed);
    }
    analyze_distinct_grouping(engine, prepared, schema, outer, params)?.apply(prepared)?;
    Ok(true)
}

/// Pre-optimization grouping identities. The stored SQL keeps its original DISTINCT clause and sets; this selection belongs only to its analyzed executable copy.
#[derive(Debug, Clone)]
pub struct AnalyzedGroupingSets {
    original_count: usize,
    retained: Vec<usize>,
}

impl AnalyzedGroupingSets {
    pub(crate) fn copy_with(
        &self,
        copying: &mut crate::plan::copy::Copying<'_>,
    ) -> Result<Self, SQLError> {
        Ok(Self {
            original_count: self.original_count,
            retained: copying.map(self.retained.iter(), |_, position| Ok(*position))?,
        })
    }

    /// Remove only sets that were equal before optimization, preserving multiplicity when distinct expressions later fold to the same value.
    pub fn apply(self, block: &mut QueryBlockPlan) -> Result<(), SQLError> {
        self.apply_with_retention(block, |sets, keep| {
            let mut index = 0;
            sets.retain(|_| {
                let retained = keep(index);
                index += 1;
                retained
            });
        })
    }

    pub(crate) fn apply_with_retention(
        self,
        block: &mut QueryBlockPlan,
        retain: impl FnOnce(&mut Vec<Vec<ScalarExpr>>, &mut dyn FnMut(usize) -> bool),
    ) -> Result<(), SQLError> {
        if !block.group_distinct || block.grouping_sets.len() != self.original_count {
            return Err(SQLError::Internal(
                "analyzed grouping selection lost its original sets".into(),
            ));
        }
        let mut retained = self.retained.into_iter().peekable();
        retain(&mut block.grouping_sets, &mut |index| {
            let keep = retained.peek() == Some(&index);
            if keep {
                retained.next();
            }
            keep
        });
        block.group_distinct = false;
        Ok(())
    }
}

pub(crate) fn analyze_distinct_grouping(
    engine: &dyn crate::routines::RoutineResolution,
    block: &QueryBlockPlan,
    schema: &RowSchema,
    outer: Option<&RowSchema>,
    params: &[SQLParam],
) -> Result<AnalyzedGroupingSets, SQLError> {
    analyze_distinct_grouping_with_control(
        engine,
        block,
        schema,
        outer,
        params,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )?
    .into_uncontrolled()
    .map_err(|_| SQLError::Internal("ordinary grouping selection owner".into()))
}

pub(crate) fn expression_identity(
    engine: &dyn FunctionTypeResolver,
    expression: &ScalarExpr,
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<Vec<u8>, SQLError> {
    expression_identity_with_control(
        engine,
        expression,
        schema,
        params,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )?
    .into_uncontrolled()
    .map_err(|_| SQLError::Internal("ordinary grouping identity owner".into()))
}

#[cfg(test)]
fn normalize_expression(
    engine: &dyn FunctionTypeResolver,
    expression: ScalarExpr,
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<ScalarExpr, SQLError> {
    let control = uqa_core::memory::ProductionControl::uncontrolled();
    normalization::Normalizer {
        engine,
        schema,
        params,
        control,
    }
    .normalize(crate::plan::OwnedScalarExpr::uncontrolled(expression))?
    .finish(&control)?
    .into_uncontrolled()
    .map_err(|_| SQLError::Internal("ordinary grouping normalization owner".into()))
}

fn input_requires_catalog(ty: &ColumnType) -> bool {
    match ty {
        ColumnType::Named(_)
        | ColumnType::Domain { .. }
        | ColumnType::Enum(_)
        | ColumnType::Composite(_)
        | ColumnType::Regproc
        | ColumnType::Regprocedure
        | ColumnType::Regclass
        | ColumnType::Regcollation
        | ColumnType::Regnamespace
        | ColumnType::Regrole
        | ColumnType::Regtype
        | ColumnType::Record
        | ColumnType::AnyArray => true,
        ColumnType::Array(element) => input_requires_catalog(element),
        _ => false,
    }
}

impl uqa_core::memory::ObserveAllocations for AnalyzedGroupingSets {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut uqa_core::memory::AllocationObserver<'a, '_>,
    ) -> Result<(), uqa_core::ValueRetentionError> {
        let Self {
            original_count: _,
            retained,
        } = self;
        observer.child(retained)?;
        Ok(())
    }
}
