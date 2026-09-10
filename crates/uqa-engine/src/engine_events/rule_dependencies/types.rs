//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Type references in stored expressions and SQL-standard routine bodies.

use super::{BTreeSet, Expr, SQLError, Statement, StoredAstVisitor};

pub(crate) fn stored_statement_relation_names(
    statement: &Statement,
) -> Result<Vec<String>, SQLError> {
    let mut names = Vec::new();
    StoredAstVisitor {
            merge: None,
            expression: None,
        ty: None,
        relation: &mut |name: &mut String| {
            names.push(name.clone());
            Ok(())
        },
        routine:
            &mut |_: &mut String, _: Option<&mut Option<uqa_sql::ast::FunctionBinding>>| Ok(()),
    }
    .bind_statement(&mut statement.clone())?;
    Ok(names)
}

pub(crate) fn stored_expression_type_names(expression: &Expr) -> Result<Vec<String>, SQLError> {
    collect_type_names(|visitor| visitor.bind_expr(&mut expression.clone(), &BTreeSet::new()))
}

pub(crate) fn stored_statement_type_names(statement: &Statement) -> Result<Vec<String>, SQLError> {
    collect_type_names(|visitor| visitor.bind_statement(&mut statement.clone()))
}

type TypeVisitor<'a> = StoredAstVisitor<
    'a,
    fn(&mut String) -> Result<(), SQLError>,
    fn(&mut String, Option<&mut Option<uqa_sql::ast::FunctionBinding>>) -> Result<(), SQLError>,
>;

fn collect_type_names(
    visit: impl FnOnce(&mut TypeVisitor<'_>) -> Result<(), SQLError>,
) -> Result<Vec<String>, SQLError> {
    let mut names = Vec::new();
    let mut relation: fn(&mut String) -> Result<(), SQLError> = |_| Ok(());
    let mut routine: fn(
        &mut String,
        Option<&mut Option<uqa_sql::ast::FunctionBinding>>,
    ) -> Result<(), SQLError> = |_, _| Ok(());
    let mut collect = |name: &mut String| names.push(name.clone());
    visit(&mut StoredAstVisitor {
        merge: None,
        expression: None,
        ty: Some(&mut collect),
        relation: &mut relation,
        routine: &mut routine,
    })?;
    Ok(names)
}
