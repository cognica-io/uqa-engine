//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Observe sequence arguments through the stored syntax and native scalar owners.

use crate::{
    ast::{ColumnType, Expr},
    catalog::work::{inline, ReferenceWork, Work},
    ir::observation::{visit_all, ScalarObserver},
    plan::PlanRef,
    SQLError, ScalarExpr,
};
use std::future::Future;
use uqa_core::{memory::ProductionControl, Value};

#[cfg(test)]
mod tests;

#[derive(Clone, Copy)]
enum Node<'a> {
    Syntax(&'a Expr),
    Scalar(&'a ScalarExpr),
}

impl<'a> Node<'a> {
    async fn reference(
        self,
        control: &ProductionControl<'_>,
        work: &mut (impl Work + Send),
    ) -> Result<Option<&'a str>, SQLError> {
        let (name, binding, mut argument) = match self {
            Self::Syntax(Expr::Func {
                name,
                binding,
                args,
                ..
            }) => (name, binding, args.first().map(Self::Syntax)),
            Self::Scalar(ScalarExpr::Func {
                name,
                binding,
                args,
                ..
            }) => (name, binding, args.first().map(Self::Scalar)),
            _ => return Ok(None),
        };
        if !super::is_sequence_function(name, binding.as_ref()) {
            return Ok(None);
        }
        while let Some(value) = argument {
            control.check()?;
            let (ty, child) = match value {
                Self::Syntax(Expr::Literal(Value::Str(value)))
                | Self::Scalar(ScalarExpr::Literal(Value::Str(value))) => return Ok(Some(value)),
                Self::Syntax(Expr::Cast { expr, ty, .. }) => (ty, Self::Syntax(expr)),
                Self::Scalar(ScalarExpr::Cast { expr, ty, .. }) => (ty, Self::Scalar(expr)),
                _ => break,
            };
            match ColumnType::from_sql_name_with_input_work(
                ty,
                control,
                &mut ReferenceWork { control, work },
            )
            .await
            {
                Ok(ty) if matches!(*ty, ColumnType::Regclass) => argument = Some(child),
                Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => {
                    return Err(error)
                }
                _ => break,
            }
        }
        Ok(None)
    }
}

pub fn visit_sequence_function_references_with_control(
    expression: &Expr,
    control: &ProductionControl<'_>,
    visit: &mut impl FnMut(&str) -> Result<(), SQLError>,
) -> Result<(), SQLError> {
    super::super::walk_schema_expr_with_control(expression, control, &mut |node| {
        if let Some(reference) = inline(Node::Syntax(node).reference(control, &mut ()))? {
            visit(reference)?;
        }
        Ok(())
    })
}

/// Observe named arguments of the selected builtin, preserving explicit text casts, typed OIDs and user-defined functions as their original runtime inputs.
pub fn visit_scalar_sequence_function_references_with_control(
    expression: &ScalarExpr,
    control: &ProductionControl<'_>,
    visit: &mut impl FnMut(&str) -> Result<(), SQLError>,
) -> Result<(), SQLError> {
    expression.try_visit(&mut |node| {
        control.check()?;
        if let Some(reference) = inline(Node::Scalar(node).reference(control, &mut ()))? {
            visit(reference)?;
        }
        Ok(true)
    })
}

/// Look up just the named argument selected from the original SQL call.
pub trait SequenceFunctionRelations: Sync {
    fn reference<W: Work + Send>(
        &self,
        reference: &str,
        work: &mut W,
    ) -> impl Future<Output = Result<(), SQLError>> + Send;
}

/// The catalog may retain schema syntax, a rule condition scalar, or a native executable plan.
pub enum StoredSequenceInput<'a> {
    Expression(&'a Expr),
    Scalar(&'a ScalarExpr),
    Plan(PlanRef<'a>),
}

pub async fn visit_stored_sequence_references_with_work<W: Work + Send>(
    source: StoredSequenceInput<'_>,
    control: &ProductionControl<'_>,
    work: &mut W,
    relations: &impl SequenceFunctionRelations,
) -> Result<(), SQLError> {
    let mut observer = Observer { control, relations };
    match source {
        StoredSequenceInput::Expression(expression) => {
            super::super::observe_schema_expression(expression, control, work, &mut observer).await
        }
        StoredSequenceInput::Scalar(expression) => {
            visit_all(expression, control, work, &mut observer).await
        }
        StoredSequenceInput::Plan(plan) => {
            crate::plan::visit_scalar_nodes_with_work(plan, control, work, &mut observer).await
        }
    }
}

struct Observer<'a, 'b, R> {
    control: &'a ProductionControl<'b>,
    relations: &'a R,
}

impl<R: SequenceFunctionRelations> super::super::SchemaObserver for Observer<'_, '_, R> {
    async fn expression<W: Work + Send>(
        &mut self,
        node: &Expr,
        work: &mut W,
    ) -> Result<(), SQLError> {
        if let Some(reference) = Node::Syntax(node).reference(self.control, work).await? {
            self.relations.reference(reference, work).await?;
        }
        Ok(())
    }
}

impl<R: SequenceFunctionRelations> ScalarObserver for Observer<'_, '_, R> {
    async fn scalar<W: Work + Send>(
        &mut self,
        node: &ScalarExpr,
        work: &mut W,
    ) -> Result<(), SQLError> {
        if let Some(reference) = Node::Scalar(node).reference(self.control, work).await? {
            self.relations.reference(reference, work).await?;
        }
        Ok(())
    }
}
