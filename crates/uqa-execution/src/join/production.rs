//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Join construction retains immutable scalar producers through physical execution.

use super::{presence, HashJoin, SpillState};
use crate::physical::construction::OperatorBuild;
use crate::scalar::PreparedExpressions;
use crate::{
    PhysicalOperator, PhysicalRow, ProjectedPredicate, RowSchema, ScalarExpr,
    SharedExpressionEvaluator,
};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};
use uqa_sql::{ast::JoinKind, ResultRow, SQLError, SQLParam};

fn simple_key_positions(
    schema: &RowSchema,
    expressions: &[ScalarExpr],
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Vec<usize>>>, SQLError> {
    let mut positions = ProductionVec::new(*control);
    for expression in expressions {
        control.check()?;
        let position = match expression {
            ScalarExpr::Column(column) => schema.position(column),
            ScalarExpr::QualifiedColumn { qualifier, column } => {
                schema.qualified_position(qualifier, column)
            }
            _ => None,
        };
        let Some(position) = position else {
            return Ok(None);
        };
        positions.push_copy(position)?;
    }
    Ok(Some(positions.finish()?))
}

fn compile_predicate(
    predicate: Option<&ScalarExpr>,
    schema: &RowSchema,
    params: Option<&[SQLParam]>,
    control: &ProductionControl<'_>,
) -> Result<Option<ProjectedPredicate>, SQLError> {
    let compiled = predicate
        .map(|predicate| {
            ProjectedPredicate::compile_with_schema_with_control(
                predicate,
                schema,
                params.unwrap_or(&[]),
                control,
            )
        })
        .transpose();
    match compiled {
        Ok(predicate) => Ok(predicate.flatten()),
        Err(error) if params.is_some() || matches!(error.sqlstate(), Some("53200" | "57014")) => {
            Err(error)
        }
        Err(_) => Ok(None),
    }
}

impl<'a> HashJoin<'a> {
    #[expect(
        clippy::too_many_arguments,
        reason = "retains join inputs, predicates and their original resource owner"
    )]
    pub fn with_owned_expressions(
        left: Box<dyn PhysicalOperator + 'a>,
        right: Box<dyn PhysicalOperator + 'a>,
        kind: JoinKind,
        left_keys: Produced<Vec<ScalarExpr>>,
        right_keys: Produced<Vec<ScalarExpr>>,
        predicate: Option<Produced<ScalarExpr>>,
        evaluator: SharedExpressionEvaluator<'a>,
        left_nulls: ResultRow,
        right_nulls: ResultRow,
        work_mem_bytes: usize,
        params: &[SQLParam],
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        Self::build_with_owned_expressions(
            left,
            right,
            kind,
            left_keys,
            right_keys,
            predicate,
            evaluator,
            left_nulls,
            right_nulls,
            work_mem_bytes,
            Some(params),
            control,
        )
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "ordinary and controlled joins share one construction boundary"
    )]
    pub(super) fn build_with_owned_expressions(
        left: Box<dyn PhysicalOperator + 'a>,
        right: Box<dyn PhysicalOperator + 'a>,
        kind: JoinKind,
        left_keys: Produced<Vec<ScalarExpr>>,
        right_keys: Produced<Vec<ScalarExpr>>,
        predicate: Option<Produced<ScalarExpr>>,
        evaluator: SharedExpressionEvaluator<'a>,
        left_nulls: ResultRow,
        right_nulls: ResultRow,
        work_mem_bytes: usize,
        params: Option<&[SQLParam]>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let (
            left,
            right,
            (
                left_keys,
                right_keys,
                left_key_positions,
                right_key_positions,
                predicate,
                prepared_predicate,
                schema,
            ),
        ) = OperatorBuild::pair(left, right, |left, right| {
            control.check()?;
            presence::join_inputs(left, right, kind, control)?;
            let left_key_positions = simple_key_positions(left.row_schema(), &left_keys, control)?;
            let right_key_positions =
                simple_key_positions(right.row_schema(), &right_keys, control)?;
            let schema = RowSchema::join_borrowed_columns_with_control(
                left.row_schema(),
                right.row_schema(),
                left_nulls
                    .keys()
                    .chain(right_nulls.keys())
                    .map(String::as_str),
                control,
            )?;
            let prepared_predicate =
                compile_predicate(predicate.as_deref(), &schema, params, control)?;
            let left_keys = PreparedExpressions::produced_scalars(left_keys, control)?;
            let right_keys = PreparedExpressions::produced_scalars(right_keys, control)?;
            let predicate = predicate
                .map(|predicate| PreparedExpressions::produced_scalar(predicate, control))
                .transpose()?;
            Ok((
                left_keys,
                right_keys,
                left_key_positions,
                right_key_positions,
                predicate,
                prepared_predicate,
                schema,
            ))
        })?;
        let left_nulls = PhysicalRow::nulls(left.row_schema().physical_width());
        let right_nulls = PhysicalRow::nulls(right.row_schema().physical_width());
        let left_cardinality = left.estimated_cardinality();
        let right_cardinality = right.estimated_cardinality();
        let build_left = matches!(kind, JoinKind::Inner)
            && left_cardinality
                .zip(right_cardinality)
                .is_some_and(|(left, right)| left < right);
        let estimated_cardinality = left_cardinality
            .zip(right_cardinality)
            .map(|(left, right)| match kind {
                JoinKind::Inner => left.max(right),
                JoinKind::Left => left,
                JoinKind::Right => right,
                JoinKind::Full => left.saturating_add(right),
                JoinKind::Cross => left.saturating_mul(right),
            });
        Ok(Self {
            left,
            right,
            kind,
            left_keys,
            right_keys,
            left_key_types: Vec::new(),
            right_key_types: Vec::new(),
            left_key_positions,
            right_key_positions,
            predicate,
            prepared_predicate,
            evaluator,
            left_nulls,
            right_nulls,
            schema,
            estimated_cardinality,
            build_left,
            work_mem_bytes,
            output: None,
            streaming_unique: None,
            streaming: None,
            probe_pending: crate::physical::demand::PendingRows::default(),
            initialized: false,
            prefetched_left: None,
            output_spilled: SpillState::InMemory,
            right_input_spilled: SpillState::InMemory,
            hash_index_spilled: SpillState::InMemory,
            memory: None,
        })
    }

    pub fn into_boxed(mut self, control: &ProductionControl<'_>) -> Result<Box<Self>, SQLError> {
        match control.reserve(size_of::<Self>()) {
            Ok(memory) => {
                self.memory = memory;
                Ok(Box::new(self))
            }
            Err(error) => {
                let left = self.left.close();
                let right = self.right.close();
                let cleanup = crate::physical::with_cleanup(
                    left,
                    right,
                    "close right hash join input after construction failure",
                );
                Err(match cleanup {
                    Ok(()) => error.into(),
                    Err(cleanup) => SQLError::from(error).with_cleanup(
                        crate::physical::physical_exec_error(cleanup),
                        "close hash join after wrapper admission failure",
                    ),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests;
