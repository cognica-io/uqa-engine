//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Public view contracts borrow declarations and admit only their lexical validation scratch.

use super::{
    ambiguous_column, ConflictActionPlan, ConflictPlan, DeletePlan, ExpressionScope, InsertPlan,
    MergePlan, MergeWhenPlan, RowSchema, SQLError, ScalarExpr, UpdatePlan, ViewRewriteContext,
};
use crate::binding::production::BindingNameSet;
use uqa_core::memory::{ProductionControl, ProductionMap};

pub(super) fn public_columns(
    services: ViewRewriteContext<'_>,
    view: &str,
    control: &ProductionControl<'_>,
) -> Result<BindingNameSet, SQLError> {
    let mut columns = BindingNameSet::new();
    services
        .catalog
        .with_view_definition_with_control(view, control, &mut |definition| {
            let definition = definition.ok_or_else(|| SQLError::UnknownTable(view.to_owned()))?;
            let schema = super::super::context::stored_view_schema_with_control(
                services, definition, control,
            )?;
            for (position, name) in schema.columns().iter().enumerate() {
                columns
                    .insert_with_control(schema.public_name(position).unwrap_or(name), control)?;
            }
            Ok(())
        })?;
    Ok(columns)
}

fn public_expression(
    expression: &ScalarExpr,
    columns: &BindingNameSet,
    scope: ExpressionScope<'_>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    reference_columns(expression, &|name| columns.contains(name), scope, control)
}

pub(in crate::semantics::view_rewrite) fn reference_columns(
    expression: &ScalarExpr,
    contains: &dyn Fn(&str) -> bool,
    scope: ExpressionScope<'_>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    expression.try_visit(&mut |node| {
        control.check()?;
        match node {
            ScalarExpr::Column(column) => {
                let target = contains(column);
                let source = scope
                    .source
                    .is_some_and(|source| source.has_unqualified_column(column));
                if target && (source || scope.include_excluded) {
                    return Err(ambiguous_column(column));
                }
                if !target && !source {
                    return Err(SQLError::UnknownColumn(column.to_string()));
                }
            }
            ScalarExpr::QualifiedColumn { qualifier, column }
                if scope.target_qualifier(qualifier) && !contains(column) =>
            {
                return Err(SQLError::UnknownColumn(format!("{qualifier}.{column}")));
            }
            _ => {}
        }
        Ok(true)
    })
}

pub(in crate::semantics::view_rewrite) fn validate_public_update_contract(
    services: ViewRewriteContext<'_>,
    plan: &UpdatePlan,
    source: Option<&RowSchema>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let columns = public_columns(services, &plan.table, control)?;
    let ordinary_scope = ExpressionScope {
        target_qualifier: &plan.target_qualifier,
        returning_aliases: None,
        source,
        include_excluded: false,
    };
    for assignment in &plan.assignments {
        for expression in assignment.expressions() {
            public_expression(expression, &columns, ordinary_scope, control)?;
        }
    }
    if let Some(predicate) = plan.predicate.as_ref() {
        public_expression(predicate, &columns, ordinary_scope, control)?;
    }
    let returning_scope = ExpressionScope {
        returning_aliases: Some(&plan.returning_aliases),
        ..ordinary_scope
    };
    for projection in &plan.returning {
        public_expression(&projection.expr, &columns, returning_scope, control)?;
    }
    Ok(())
}

pub(in crate::semantics::view_rewrite) fn validate_public_delete_contract(
    services: ViewRewriteContext<'_>,
    plan: &DeletePlan,
    source: Option<&RowSchema>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let columns = public_columns(services, &plan.table, control)?;
    let ordinary_scope = ExpressionScope {
        target_qualifier: &plan.target_qualifier,
        returning_aliases: None,
        source,
        include_excluded: false,
    };
    if let Some(predicate) = plan.predicate.as_ref() {
        public_expression(predicate, &columns, ordinary_scope, control)?;
    }
    let returning_scope = ExpressionScope {
        returning_aliases: Some(&plan.returning_aliases),
        ..ordinary_scope
    };
    for projection in &plan.returning {
        public_expression(&projection.expr, &columns, returning_scope, control)?;
    }
    Ok(())
}

pub(in crate::semantics::view_rewrite) fn validate_public_insert_contract(
    services: ViewRewriteContext<'_>,
    plan: &InsertPlan,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let columns = public_columns(services, &plan.table, control)?;
    for predicate in plan.on_conflict.iter().flat_map(|conflict| {
        conflict
            .expressions
            .iter()
            .chain(conflict.predicate.iter().map(Box::as_ref))
    }) {
        public_expression(
            predicate,
            &columns,
            ExpressionScope {
                target_qualifier: &plan.target_qualifier,
                returning_aliases: None,
                source: None,
                include_excluded: false,
            },
            control,
        )?;
    }
    if let Some(ConflictPlan {
        action:
            ConflictActionPlan::Update {
                assignments,
                predicate,
            },
        ..
    }) = &plan.on_conflict
    {
        let scope = ExpressionScope {
            target_qualifier: &plan.target_qualifier,
            returning_aliases: None,
            source: None,
            include_excluded: true,
        };
        for assignment in assignments {
            for expression in assignment.expressions() {
                public_expression(expression, &columns, scope, control)?;
            }
        }
        if let Some(predicate) = predicate {
            public_expression(predicate, &columns, scope, control)?;
        }
    }
    let scope = ExpressionScope {
        target_qualifier: &plan.target_qualifier,
        returning_aliases: Some(&plan.returning_aliases),
        source: None,
        include_excluded: false,
    };
    for projection in &plan.returning {
        public_expression(&projection.expr, &columns, scope, control)?;
    }
    Ok(())
}

pub fn validate_public_merge_contract(
    services: ViewRewriteContext<'_>,
    plan: &MergePlan,
    source: &RowSchema,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let columns = public_columns(services, &plan.target, control)?;
    let matched_scope = ExpressionScope {
        target_qualifier: &plan.target_qualifier,
        returning_aliases: None,
        source: Some(source),
        include_excluded: false,
    };
    let target_only_scope = ExpressionScope {
        source: None,
        ..matched_scope
    };
    public_expression(&plan.join_condition, &columns, matched_scope, control)?;
    for clause in &plan.when_clauses {
        match clause {
            MergeWhenPlan::UpdateMatched {
                condition,
                assignments,
            } => {
                if let Some(condition) = condition {
                    public_expression(condition, &columns, matched_scope, control)?;
                }
                for assignment in assignments {
                    for expression in assignment.expressions() {
                        public_expression(expression, &columns, matched_scope, control)?;
                    }
                }
            }
            MergeWhenPlan::DeleteMatched { condition }
            | MergeWhenPlan::NothingMatched { condition } => {
                if let Some(condition) = condition {
                    public_expression(condition, &columns, matched_scope, control)?;
                }
            }
            MergeWhenPlan::UpdateNotMatchedBySource {
                condition,
                assignments,
            } => {
                if let Some(condition) = condition {
                    public_expression(condition, &columns, target_only_scope, control)?;
                }
                for assignment in assignments {
                    public_expression(&assignment.value, &columns, target_only_scope, control)?;
                }
            }
            MergeWhenPlan::DeleteNotMatchedBySource { condition }
            | MergeWhenPlan::NothingNotMatchedBySource { condition } => {
                if let Some(condition) = condition {
                    public_expression(condition, &columns, target_only_scope, control)?;
                }
            }
            MergeWhenPlan::InsertNotMatched { .. } | MergeWhenPlan::NothingNotMatched { .. } => {}
        }
    }
    let returning_scope = ExpressionScope {
        returning_aliases: Some(&plan.returning_aliases),
        ..matched_scope
    };
    for projection in &plan.returning {
        public_expression(&projection.expr, &columns, returning_scope, control)?;
    }
    Ok(())
}

pub(in crate::semantics::view_rewrite) fn validate_mapped_columns<'a>(
    targets: impl IntoIterator<Item = &'a crate::ast::AssignmentTarget<ScalarExpr>>,
    duplicate: fn(&str) -> SQLError,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let mut seen = ProductionMap::new(*control);
    for target in targets {
        control.check()?;
        let whole = target.is_whole_column();
        if seen
            .insert_copy(target.column.as_str(), whole)?
            .is_some_and(|previous| previous || whole)
        {
            return Err(duplicate(&target.column));
        }
    }
    Ok(())
}

pub(in crate::semantics::view_rewrite) fn validate_public_view_targets<'a>(
    services: ViewRewriteContext<'_>,
    view: &str,
    columns: impl IntoIterator<Item = &'a str>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let mut schema = None;
    services
        .catalog
        .with_view_definition_with_control(view, control, &mut |definition| {
            let definition = definition.ok_or_else(|| SQLError::UnknownTable(view.to_owned()))?;
            schema = Some(super::super::context::stored_view_schema_with_control(
                services, definition, control,
            )?);
            Ok(())
        })?;
    let schema = schema.expect("catalog visits the selected view");
    for column in columns {
        control.check()?;
        if !schema.has_unqualified_column(column) {
            return Err(SQLError::UnknownColumn(format!("{view}.{column}")));
        }
    }
    Ok(())
}

pub(in crate::semantics::view_rewrite) fn view_targets<'a>(
    layer: &super::AutomaticViewLayer,
    targets: impl IntoIterator<Item = &'a crate::ast::AssignmentTarget<ScalarExpr>>,
    duplicate: fn(&str) -> SQLError,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let mut seen = ProductionMap::new(*control);
    for target in targets {
        control.check()?;
        if super::layer_column(layer, &target.column).is_none() {
            return Err(super::unknown_view_column(layer, &target.column));
        }
        let whole = target.is_whole_column();
        if seen
            .insert_copy(target.column.as_str(), whole)?
            .is_some_and(|previous| previous || whole)
        {
            return Err(duplicate(&target.column));
        }
    }
    Ok(())
}

pub(in crate::semantics::view_rewrite) fn writable_column(
    layer: &super::AutomaticViewLayer,
    name: &str,
    write: super::ColumnWrite,
    control: &ProductionControl<'_>,
) -> Result<uqa_core::memory::Produced<String>, SQLError> {
    control.check()?;
    let column =
        super::layer_column(layer, name).ok_or_else(|| super::unknown_view_column(layer, name))?;
    let name = column.writable_source_column.as_deref().ok_or_else(|| {
        super::non_writable_column(
            &layer.canonical_name,
            name,
            write,
            layer.column_restriction(column),
        )
    })?;
    Ok(control.copy_text(name)?)
}

pub(in crate::semantics::view_rewrite) fn writable_columns<'a>(
    layer: &super::AutomaticViewLayer,
    modified: impl IntoIterator<Item = &'a str>,
    write: super::ColumnWrite,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let mut columns = ProductionMap::new(*control);
    for name in modified {
        columns.insert_copy(name, ())?;
    }
    for column in &layer.columns {
        control.check()?;
        if column.writable_source_column.is_none() && columns.get(&column.name.as_str()).is_some() {
            return Err(super::non_writable_column(
                &layer.canonical_name,
                &column.name,
                write,
                layer.column_restriction(column),
            ));
        }
    }
    Ok(())
}

pub(in crate::semantics::view_rewrite) fn insert_targets(
    layer: &super::AutomaticViewLayer,
    plan: &InsertPlan,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    view_targets(
        layer,
        plan.columns.iter(),
        super::duplicate_insert_column,
        control,
    )?;
    let Some(conflict) = &plan.on_conflict else {
        return Ok(());
    };
    for column in &conflict.conflict_columns {
        control.check()?;
        if super::layer_column(layer, column).is_none() {
            return Err(super::unknown_view_column(layer, column));
        }
    }
    if let ConflictActionPlan::Update { assignments, .. } = &conflict.action {
        view_targets(
            layer,
            assignments
                .iter()
                .flat_map(|assignment| assignment.target.targets()),
            super::duplicate_assignment,
            control,
        )?;
    }
    Ok(())
}
