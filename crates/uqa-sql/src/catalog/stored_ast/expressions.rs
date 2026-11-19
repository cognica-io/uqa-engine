//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Expression traversal shared by stored statement dependency binding.

use super::{
    BTreeSet, Expr, ExpressionCallback, ProjectionCallback, SQLError, Statement, StoredAstVisitor,
};

struct Expressions<F>(F);
impl<W, F: FnMut(&mut Expr) -> Result<(), SQLError>> super::Actions<W> for Expressions<F> {
    fn expression(
        &mut self,
        expr: &mut Expr,
        _: &uqa_core::memory::ProductionControl<'_>,
        _: &mut W,
    ) -> impl std::future::Future<Output = Result<(), SQLError>> + Send {
        std::future::ready((self.0)(expr))
    }
}

pub fn visit_stored_expression(
    expression: &mut Expr,
    visit: ExpressionCallback<'_>,
) -> Result<(), SQLError> {
    crate::catalog::work::inline(visit_stored_expression_from(
        expression,
        visit,
        &uqa_core::memory::ProductionControl::uncontrolled(),
        &mut (),
    ))
}
pub(crate) async fn visit_stored_expression_from<W: super::Work + Send>(
    expression: &mut Expr,
    visit: impl FnMut(&mut Expr) -> Result<(), SQLError>,
    control: &uqa_core::memory::ProductionControl<'_>,
    work: &mut W,
) -> Result<(), SQLError> {
    super::Reader::new(&mut Expressions(visit), control, work)
        .bind_expr(expression, &BTreeSet::new())
        .await
}

pub fn visit_stored_statement_expressions(
    statement: &mut Statement,
    visit: ExpressionCallback<'_>,
) -> Result<(), SQLError> {
    crate::catalog::work::inline(visit_stored_statement_expressions_from(
        statement,
        visit,
        &uqa_core::memory::ProductionControl::uncontrolled(),
        &mut (),
    ))
}
pub(crate) async fn visit_stored_statement_expressions_from<W: super::Work + Send>(
    statement: &mut Statement,
    visit: impl FnMut(&mut Expr) -> Result<(), SQLError>,
    control: &uqa_core::memory::ProductionControl<'_>,
    work: &mut W,
) -> Result<(), SQLError> {
    super::Reader::new(&mut Expressions(visit), control, work)
        .bind_statement(statement)
        .await
}

/// Visit each select list or `RETURNING` item of a stored statement before its expression, and each expression in the order of the other visits.
pub fn visit_stored_statement_projections(
    statement: &mut Statement,
    projection: ProjectionCallback<'_>,
    expression: ExpressionCallback<'_>,
) -> Result<(), SQLError> {
    StoredAstVisitor {
        source: None,
        merge: None,
        expression: Some(expression),
        projection: Some(projection),
        ty: None,
        relation: &mut |_: &mut String| Ok(()),
        routine: &mut |_: &mut String, _: Option<&mut Option<crate::ast::FunctionBinding>>| Ok(()),
    }
    .bind_statement(statement)
}

/// Whether two stored expressions define the same expression apart from the input collations their calls record. A definition stored before calls recorded them records none; an explicit COLLATE remains a node of its own, and the same call over one relation derives the same selection.
#[must_use]
pub fn same_stored_expression(left: &Expr, right: &Expr) -> bool {
    left == right
        || matches!(
            (without_input_collations(left), without_input_collations(right)),
            (Ok(left), Ok(right)) if left == right
        )
}

fn without_input_collations(expression: &Expr) -> Result<Expr, SQLError> {
    let mut expression = expression.clone();
    StoredAstVisitor {
        source: None,
        merge: None,
        expression: None,
        projection: None,
        ty: None,
        relation: &mut |_: &mut String| Ok(()),
        routine:
            &mut |_: &mut String, binding: Option<&mut Option<crate::ast::FunctionBinding>>| {
                if let Some(Some(binding)) = binding {
                    binding.input_collation = None;
                }
                Ok(())
            },
    }
    .bind_expr(&mut expression, &BTreeSet::new())?;
    Ok(expression)
}
