//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored statement and rewrite-rule column dependency entry points.

use super::{
    BTreeSet, ColumnBindingContext, ColumnBindingMode, Expr, RelationIdentity,
    RuleColumnDependency, SQLError, Statement, StoredColumnBinder, StoredColumnBindingContext,
};

pub fn bind_stored_statement_source_columns(
    statement: &mut Statement,
    mut source_columns: impl FnMut(&str) -> Result<Option<Vec<String>>, SQLError>,
) -> Result<bool, SQLError> {
    let mut changed = false;
    crate::catalog::stored_ast::visit_stored_statement_sources(statement, &mut |source| {
        let crate::ast::FromClause::Table {
            name,
            bound_columns,
            ..
        } = source
        else {
            return Ok(());
        };
        if bound_columns.is_some() {
            return Ok(());
        }
        *bound_columns = source_columns(name)?;
        changed |= bound_columns.is_some();
        Ok(())
    })?;
    Ok(changed)
}

pub fn remove_stored_statement_source_column_aliases(
    catalog: StoredColumnBindingContext<'_>,
    statement: &mut Statement,
    dependencies: &BTreeSet<RuleColumnDependency>,
) -> Result<bool, SQLError> {
    let mut planned = statement.clone();
    let mut binder = StoredColumnBinder::new(catalog, ColumnBindingMode::Drop { dependencies });
    binder.bind_statement(&mut planned, &[], &ColumnBindingContext::default())?;
    let changed = binder.alias_shape_changed();
    if changed {
        crate::catalog::stored_ast::copy_stored_source_column_shapes(&mut planned, statement)?;
    }
    Ok(changed)
}

pub fn stored_statement_column_dependencies(
    catalog: StoredColumnBindingContext<'_>,
    statement: &Statement,
) -> Result<BTreeSet<RuleColumnDependency>, SQLError> {
    bind_rule_action_column_dependencies(catalog, &mut statement.clone())
}

pub fn bind_rule_condition_column_dependencies(
    catalog: StoredColumnBindingContext<'_>,
    condition: &mut Expr,
) -> Result<BTreeSet<RuleColumnDependency>, SQLError> {
    let mut binder = StoredColumnBinder::new(catalog, ColumnBindingMode::Bind);
    binder.bind_expr(condition, &[], &ColumnBindingContext::default())?;
    Ok(binder.finish())
}

pub fn bind_rule_action_column_dependencies(
    catalog: StoredColumnBindingContext<'_>,
    action: &mut Statement,
) -> Result<BTreeSet<RuleColumnDependency>, SQLError> {
    let mut binder = StoredColumnBinder::new(catalog, ColumnBindingMode::Bind);
    binder.bind_statement(action, &[], &ColumnBindingContext::default())?;
    Ok(binder.finish())
}

pub fn rewrite_rule_column_references(
    catalog: StoredColumnBindingContext<'_>,
    definition: &mut crate::ast::CreateRule,
    relation: &RelationIdentity,
    from: &str,
    to: &str,
) -> Result<(), SQLError> {
    let mode = ColumnBindingMode::Rename { relation, from, to };
    if let Some(condition) = &mut definition.condition {
        let mut binder = StoredColumnBinder::new(catalog, mode);
        binder.bind_expr(condition, &[], &ColumnBindingContext::default())?;
    }
    for action in &mut definition.actions {
        let mut binder = StoredColumnBinder::new(catalog, mode);
        binder.bind_statement(action, &[], &ColumnBindingContext::default())?;
    }
    Ok(())
}

pub fn remove_rule_source_column_aliases(
    catalog: StoredColumnBindingContext<'_>,
    definition: &mut crate::ast::CreateRule,
    dependency: &RuleColumnDependency,
) -> Result<bool, SQLError> {
    let dependencies = BTreeSet::from([dependency.clone()]);
    let mode = ColumnBindingMode::Drop {
        dependencies: &dependencies,
    };
    let mut changed = false;
    if let Some(condition) = &mut definition.condition {
        let mut binder = StoredColumnBinder::new(catalog, mode);
        binder.bind_expr(condition, &[], &ColumnBindingContext::default())?;
        changed |= binder.alias_shape_changed();
    }
    for action in &mut definition.actions {
        let mut binder = StoredColumnBinder::new(catalog, mode);
        binder.bind_statement(action, &[], &ColumnBindingContext::default())?;
        changed |= binder.alias_shape_changed();
    }
    Ok(changed)
}
