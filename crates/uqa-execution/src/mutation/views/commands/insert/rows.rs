//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Positional view assignments, row-trigger calls and bounded RETURNING ownership.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use uqa_core::Value;
use uqa_sql::ast::{AssignmentTarget, ColumnType};
use uqa_sql::{plan::InsertPlan, SQLError, SQLParam, ScalarExpr};

use crate::mutation::assignment::{MutationAssignmentContext, TypedAssignmentTarget};
use crate::mutation::constraints::ConstraintContext;
use crate::mutation::preparation::MutationPreparationContext;
use crate::mutation::returning::{ReturningRows, ReturningValueProjectionRow};
use crate::mutation::statement::context::MutationStatementContext;
use crate::query::consumer::{QueryConsumerControl, QueryRowConsumer};
use crate::query::{runtime::QueryRuntimeView, CteScope};
use crate::{OwnedPhysicalRow, RowSchema};

use super::super::{
    build_returning_value_row, eval_mutation_expr, finish_view_dml, DmlReturningShape,
    MutationOutput, TriggerViewChecks, ViewDmlTarget,
};

pub(super) struct ViewInsertRows<'a, S: Clone + 'static> {
    live: MutationPreparationContext<'a, S>,
    read: MutationAssignmentContext<'a, S>,
    constraints: ConstraintContext<'a>,
    runtime: QueryRuntimeView<'a>,
    target: &'a ViewDmlTarget,
    stmt: &'a InsertPlan,
    params: &'a [SQLParam],
    scope: &'a CteScope<S>,
    supplied: uqa_core::memory::Produced<Vec<String>>,
    affected: Cell<u64>,
    returning: RefCell<Option<ReturningRows>>,
}

impl<'a, S: Clone + 'static> ViewInsertRows<'a, S> {
    pub(super) fn new(
        context: &MutationStatementContext<'a, S>,
        read: &MutationStatementContext<'a, S>,
        target: &'a ViewDmlTarget,
        stmt: &'a InsertPlan,
        params: &'a [SQLParam],
        scope: &'a CteScope<S>,
    ) -> Result<Self, SQLError> {
        let resources = context.query.source.ctes.production_resources()?;
        Ok(Self {
            live: context.mutation.preparation,
            read: read.mutation.preparation.referential.assignment,
            constraints: read.mutation.preparation.referential.constraints,
            runtime: context.query.source.relational.runtime,
            target,
            stmt,
            params,
            scope,
            supplied:
                uqa_sql::semantics::mutation_privileges::insert_supplied_columns_with_control(
                    stmt,
                    |read| {
                        for name in &target.columns {
                            read(name)?;
                        }
                        Ok(())
                    },
                    &resources.control(),
                )?,
            affected: Cell::new(0),
            returning: RefCell::new(None),
        })
    }

    pub(super) fn project(
        &self,
        columns: &[AssignmentTarget<ScalarExpr>],
        positions: &[usize],
        width: usize,
        input: impl IntoIterator<Item = Result<(Value, Option<ColumnType>), SQLError>>,
    ) -> Result<Vec<Value>, SQLError> {
        self.runtime.check_cancelled()?;
        if width > columns.len() || (!self.stmt.columns.is_empty() && width != columns.len()) {
            return Err(SQLError::TypeMismatch(format!(
                "row width {width} != column count {}",
                columns.len()
            )));
        }
        let mut new = vec![Value::Null; self.target.columns.len()];
        for (input_position, (target_position, input)) in
            positions.iter().copied().zip(input).enumerate()
        {
            let (value, source) = input?;
            new[target_position] = crate::mutation::assignment::coerce_typed_assignment(
                self.read,
                self.scope,
                TypedAssignmentTarget {
                    target: &columns[input_position],
                    ty: self.target.schema.column_type(target_position),
                    current: Some(&new[target_position]),
                    final_column_write: !columns[input_position + 1..]
                        .iter()
                        .any(|next| next.column == columns[input_position].column),
                },
                value,
                source.as_ref(),
                None,
                self.params,
            )?;
        }
        Ok(new)
    }

    pub(super) fn values(
        &self,
        columns: &[AssignmentTarget<ScalarExpr>],
        positions: &[usize],
        rows: &[Vec<ScalarExpr>],
    ) -> Result<(), SQLError> {
        let snapshot = self.scope.returning_statement_snapshot_scope();
        let hook = self
            .read
            .expressions
            .expressions
            .bind_scope(snapshot.clone());
        let schema = RowSchema::default();
        for expressions in rows {
            let input = expressions.iter().map(|expression| {
                if matches!(expression, ScalarExpr::Default) {
                    return Ok((Value::Null, None));
                }
                let source = uqa_sql::type_resolution::assignment_source_type(
                    expression,
                    &schema,
                    self.params,
                    hook.as_ref(),
                )?;
                let value = eval_mutation_expr(
                    self.read.expressions,
                    &snapshot,
                    expression,
                    None,
                    self.params,
                )?;
                Ok((value, source))
            });
            let new = self.project(columns, positions, expressions.len(), input)?;
            self.apply(&new)?;
        }
        Ok(())
    }

    pub(super) fn apply(&self, new: &[Value]) -> Result<(), SQLError> {
        self.runtime.check_cancelled()?;
        let Some(final_new) = crate::mutation::triggers::fire_instead_of_row_triggers(
            &self.live.referential.triggers,
            &self.target.canonical_name,
            uqa_sql::ast::TriggerEvent::Insert,
            None,
            Some(new),
            &[],
        )?
        else {
            return Ok(());
        };
        TriggerViewChecks {
            expressions: self.read.expressions,
            constraints: self.constraints,
            target: self.target,
            target_qualifier: &self.stmt.target_qualifier,
            checks: &self.stmt.view_checks,
            supplied: &self.supplied,
            params: self.params,
            scope: self.scope,
        }
        .validate(&final_new)?;
        self.affected.set(self.affected.get() + 1);
        if !self.stmt.returning.is_empty() {
            let row = build_returning_value_row(
                self.live.returning,
                ReturningValueProjectionRow {
                    table: &self.target.canonical_name,
                    target_qualifier: &self.stmt.target_qualifier,
                    current: &final_new,
                    old: None,
                    new: Some(&final_new),
                    aliases: &self.stmt.returning_aliases,
                    context: None,
                },
                &self.stmt.returning,
                self.params,
                self.scope,
            )?;
            let mut returning = self.returning.borrow_mut();
            if returning.is_none() {
                *returning = Some(ReturningRows::new(self.runtime)?);
            }
            returning
                .as_mut()
                .expect("initialized returning owner")
                .push_to(row, self.live.returning.destination)?;
        }
        Ok(())
    }

    pub(super) fn finish(&self) -> Result<MutationOutput, SQLError> {
        let rows = self
            .returning
            .borrow_mut()
            .take()
            .map_or_else(|| ReturningRows::new(self.runtime), Ok)?;
        finish_view_dml(
            &self.live.returning,
            DmlReturningShape {
                table: &self.target.canonical_name,
                target_qualifier: &self.stmt.target_qualifier,
                aliases: &self.stmt.returning_aliases,
                returning: &self.stmt.returning,
                params: self.params,
                ctes: self.scope,
                supplemental_schema: None,
            },
            rows,
            self.affected.get(),
        )
    }
}

pub(super) struct ViewInsertSource<'a, S: Clone + 'static> {
    pub(super) rows: Rc<ViewInsertRows<'a, S>>,
    pub(super) columns: &'a [AssignmentTarget<ScalarExpr>],
    pub(super) positions: &'a [usize],
    pub(super) width: Cell<usize>,
}

impl<S: Clone + 'static> QueryRowConsumer for ViewInsertSource<'_, S> {
    fn begin(&self, columns: &[String], _schema: &RowSchema) -> Result<(), SQLError> {
        self.width.set(columns.len());
        Ok(())
    }

    fn consume(&self, row: OwnedPhysicalRow) -> Result<QueryConsumerControl, SQLError> {
        let view = row.view();
        let input = (0..self.width.get()).map(|position| {
            let value = view.value_at(position).cloned().ok_or_else(|| {
                SQLError::Internal("view INSERT source omitted a public slot".into())
            })?;
            Ok((value, row.schema.column_type(position).cloned()))
        });
        let new = self
            .rows
            .project(self.columns, self.positions, self.width.get(), input)?;
        self.rows.apply(&new)?;
        Ok(QueryConsumerControl::Continue)
    }
}
