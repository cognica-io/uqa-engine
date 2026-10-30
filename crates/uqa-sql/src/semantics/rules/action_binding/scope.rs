//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL source-name and shadowing scopes for rewrite-rule binding.

use super::{
    names::{self, Columns, Named, Names},
    RuleBindingContext,
};
use crate::{
    ast::{Expr, FromClause, SelectStmt},
    SQLError,
};
use std::collections::BTreeSet;
use uqa_core::memory::ProductionControl;

#[derive(Default)]
pub(super) struct RuleBindingScope {
    qualifiers: Names,
    columns: Names,
}
struct RuleSourceNames {
    columns: Columns,
    qualified_columns: Named<Columns>,
}
impl RuleSourceNames {
    fn new(control: &ProductionControl<'_>) -> Self {
        Self {
            columns: Columns::new(control),
            qualified_columns: Named::new(),
        }
    }
    fn insert_qualifier(
        &mut self,
        qualifier: &str,
        columns: &Columns,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        names::insert(
            &mut self.qualified_columns,
            qualifier,
            columns.copy(control)?,
            control,
        )
    }
    fn qualified(&self, qualifier: &str) -> Option<&Columns> {
        self.qualified_columns
            .get_by(|key| names::folded_cmp(qualifier, key))
    }
}
impl RuleBindingScope {
    pub(super) fn from_qualifiers_with_control(
        qualifiers: &BTreeSet<String>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let mut scope = Self::default();
        for qualifier in qualifiers {
            scope.insert_qualifier(qualifier, control)?;
        }
        Ok(scope)
    }
    pub(super) fn copy(&self, control: &ProductionControl<'_>) -> Result<Self, SQLError> {
        let mut scope = Self::default();
        for qualifier in &self.qualifiers {
            scope.insert_qualifier(qualifier, control)?;
        }
        for column in &self.columns {
            scope.insert_column(column, control)?;
        }
        Ok(scope)
    }
    pub(super) fn insert_qualifier(
        &mut self,
        qualifier: &str,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        names::insert_name(&mut self.qualifiers, qualifier, true, control)
    }
    fn insert_column(
        &mut self,
        column: &str,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        names::insert_name(&mut self.columns, column, true, control)
    }
    pub(super) fn qualifier_is_shadowed(&self, qualifier: &str) -> bool {
        self.qualifiers
            .contains_by(|key| names::folded_cmp(qualifier, key))
    }
    pub(super) fn column_is_shadowed(&self, column: &str) -> bool {
        self.columns
            .contains_by(|key| names::folded_cmp(column, key))
    }
}

pub(super) fn collect_visible_scope(
    from: &FromClause,
    context: &RuleBindingContext<'_>,
    output: &mut RuleBindingScope,
) -> Result<(), SQLError> {
    let names = source_names(from, context)?;
    let control = context.control();
    for qualifier in names.qualified_columns.keys() {
        output.insert_qualifier(qualifier, &control)?;
    }
    for column in names.columns.iter() {
        output.insert_column(column, &control)?;
    }
    Ok(())
}

fn source_names(
    from: &FromClause,
    context: &RuleBindingContext<'_>,
) -> Result<RuleSourceNames, SQLError> {
    let control = context.control();
    control.check()?;
    match from {
        FromClause::RetainedRows(_) => Ok(RuleSourceNames::new(&control)),
        FromClause::Table {
            name,
            qualifier,
            alias,
            column_aliases,
            ..
        } => table_source_names(context, name, qualifier, alias.as_deref(), column_aliases),
        FromClause::Join {
            left,
            right,
            alias,
            column_aliases,
            using,
            natural,
            ..
        } => join_source_names(
            context,
            left,
            right,
            alias.as_deref(),
            column_aliases,
            using.as_ref(),
            *natural,
        ),
        FromClause::Values {
            rows,
            alias,
            column_aliases,
            ..
        } => aliased_source_names(
            value_columns(rows.first().map_or(0, Vec::len), &control)?,
            alias.as_deref(),
            column_aliases,
            &control,
        ),
        FromClause::Subquery {
            body,
            alias,
            column_aliases,
            ..
        } => aliased_source_names(
            select_output_columns(body, context)?,
            alias.as_deref(),
            column_aliases,
            &control,
        ),
        FromClause::Function {
            output_name,
            alias,
            column_aliases,
            ordinality,
            ..
        } => {
            let mut columns = Columns::from_names([output_name.as_str()], &control)?;
            columns.rename(column_aliases, &control)?;
            if *ordinality {
                columns.push("ordinality", &control)?;
            }
            let mut names = RuleSourceNames::new(&control);
            names.columns = columns.copy(&control)?;
            names.insert_qualifier(alias.as_deref().unwrap_or(output_name), &columns, &control)?;
            Ok(names)
        }
        FromClause::FunctionGroup {
            functions,
            alias,
            column_aliases,
            ordinality,
        } => function_group_source_names(
            functions,
            alias.as_deref(),
            column_aliases,
            *ordinality,
            &control,
        ),
    }
}

fn table_source_names(
    context: &RuleBindingContext<'_>,
    name: &str,
    qualifier: &str,
    alias: Option<&str>,
    column_aliases: &[String],
) -> Result<RuleSourceNames, SQLError> {
    let control = context.control();
    let mut columns = context.relation_columns(name)?;
    columns.rename(column_aliases, &control)?;
    let mut names = RuleSourceNames::new(&control);
    names.columns = columns.copy(&control)?;
    if let Some(alias) = alias {
        names.insert_qualifier(alias, &columns, &control)?;
    } else {
        names.insert_qualifier(qualifier, &columns, &control)?;
        names.insert_qualifier(name, &columns, &control)?;
        if let Some((_, local)) = name.rsplit_once('.') {
            names.insert_qualifier(local.trim_matches('"'), &columns, &control)?;
        }
    }
    Ok(names)
}

fn function_group_source_names(
    functions: &[crate::ast::TableFunction],
    alias: Option<&str>,
    column_aliases: &[String],
    ordinality: bool,
    control: &ProductionControl<'_>,
) -> Result<RuleSourceNames, SQLError> {
    let mut columns = Columns::new(control);
    for function in functions {
        columns.extend(function_output_columns(function), control)?;
    }
    columns.rename(column_aliases, control)?;
    if ordinality {
        columns.push("ordinality", control)?;
    }
    let mut names = RuleSourceNames::new(control);
    names.columns = columns.copy(control)?;
    if let Some(alias) = alias {
        names.insert_qualifier(alias, &columns, control)?;
    } else {
        for function in functions {
            names.insert_qualifier(
                &function.output_name,
                &Columns::from_names(function_output_columns(function), control)?,
                control,
            )?;
        }
    }
    Ok(names)
}

fn value_columns(width: usize, control: &ProductionControl<'_>) -> Result<Columns, SQLError> {
    let mut columns = Columns::new(control);
    for position in 1..=width {
        columns.push_produced(control.format(format_args!("column{position}"))?, control)?;
    }
    Ok(columns)
}
fn join_source_names(
    context: &RuleBindingContext<'_>,
    left: &FromClause,
    right: &FromClause,
    alias: Option<&str>,
    column_aliases: &[String],
    using: Option<&crate::ast::JoinUsing>,
    natural: bool,
) -> Result<RuleSourceNames, SQLError> {
    let control = context.control();
    let left = source_names(left, context)?;
    let right = source_names(right, context)?;
    let merged = if natural {
        Columns::from_names(
            left.columns
                .iter()
                .filter(|column| contains_name(&right.columns, column))
                .map(|name| name.as_str()),
            &control,
        )?
    } else {
        Columns::from_names(
            using
                .into_iter()
                .flat_map(|using| using.columns.iter().map(String::as_str)),
            &control,
        )?
    };
    let mut columns = merged.copy(&control)?;
    columns.extend(
        left.columns
            .iter()
            .filter(|column| !contains_name(&merged, column))
            .map(|name| name.as_str()),
        &control,
    )?;
    columns.extend(
        right
            .columns
            .iter()
            .filter(|column| !contains_name(&merged, column))
            .map(|name| name.as_str()),
        &control,
    )?;
    columns.rename(column_aliases, &control)?;
    let mut names = RuleSourceNames::new(&control);
    names.columns = columns.copy(&control)?;
    if let Some(alias) = alias {
        names.insert_qualifier(alias, &columns, &control)?;
    } else {
        names.qualified_columns.append(left.qualified_columns);
        names.qualified_columns.append(right.qualified_columns);
    }
    if let Some(alias) = using.and_then(|using| using.alias.as_deref()) {
        names.insert_qualifier(alias, &merged, &control)?;
    }
    Ok(names)
}
fn aliased_source_names(
    mut columns: Columns,
    alias: Option<&str>,
    aliases: &[String],
    control: &ProductionControl<'_>,
) -> Result<RuleSourceNames, SQLError> {
    columns.rename(aliases, control)?;
    let mut names = RuleSourceNames::new(control);
    names.columns = columns.copy(control)?;
    if let Some(alias) = alias {
        names.insert_qualifier(alias, &columns, control)?;
    }
    Ok(names)
}
fn function_output_columns(function: &crate::ast::TableFunction) -> impl Iterator<Item = &str> {
    let names = if function.column_aliases.is_empty() {
        std::slice::from_ref(&function.output_name)
    } else {
        &function.column_aliases
    };
    names.iter().map(String::as_str)
}

pub(super) fn cte_output_columns(
    body: &crate::ast::CteBody,
    context: &RuleBindingContext<'_>,
) -> Result<Columns, SQLError> {
    use crate::ast::CteBody;
    let (table, qualifier, aliases, returning, from, ctes) = match body {
        CteBody::Query(query) => return select_output_columns(query, context),
        CteBody::Insert(plan) => (
            &plan.table,
            &plan.target_qualifier,
            &plan.returning_aliases,
            &plan.returning,
            None,
            &plan.with,
        ),
        CteBody::Update(plan) => (
            &plan.table,
            &plan.target_qualifier,
            &plan.returning_aliases,
            &plan.returning,
            plan.from.as_ref(),
            &plan.with,
        ),
        CteBody::Delete(plan) => (
            &plan.table,
            &plan.target_qualifier,
            &plan.returning_aliases,
            &plan.returning,
            plan.using.as_ref(),
            &plan.with,
        ),
        CteBody::Merge(plan) => (
            &plan.target,
            &plan.target_qualifier,
            &plan.returning_aliases,
            &plan.returning,
            Some(&plan.source),
            &plan.with,
        ),
    };
    let context = context.with_ctes(ctes)?;
    let control = context.control();
    let columns = context.relation_columns(table)?;
    let mut source = from
        .map(|from| source_names(from, &context))
        .transpose()?
        .unwrap_or_else(|| RuleSourceNames::new(&control));
    source.columns = columns.copy(&control)?;
    source.insert_qualifier(qualifier, &columns, &control)?;
    source.insert_qualifier(&aliases.old, &columns, &control)?;
    source.insert_qualifier(&aliases.new, &columns, &control)?;
    projection_output_columns(returning, &source, &control)
}
pub(super) fn select_output_columns(
    select: &SelectStmt,
    context: &RuleBindingContext<'_>,
) -> Result<Columns, SQLError> {
    let context = context.with_ctes(&select.with)?;
    let control = context.control();
    if let Some(left) = select.set_op.as_ref().and_then(|set| set.left.as_deref()) {
        return select_output_columns(left, &context);
    }
    if select.projections.is_empty() && !select.values.is_empty() {
        return value_columns(select.values.first().map_or(0, Vec::len), &control);
    }
    let source = select
        .from
        .as_ref()
        .map(|source| source_names(source, &context))
        .transpose()?
        .unwrap_or_else(|| RuleSourceNames::new(&control));
    projection_output_columns(&select.projections, &source, &control)
}
fn projection_output_columns(
    projections: &[crate::ast::Projection],
    source: &RuleSourceNames,
    control: &ProductionControl<'_>,
) -> Result<Columns, SQLError> {
    let mut columns = Columns::new(control);
    for projection in projections {
        control.check()?;
        if let Some(alias) = &projection.alias {
            columns.push(alias, control)?;
            continue;
        }
        match &projection.expr {
            Expr::Star => {
                columns.extend(source.columns.iter().map(|name| name.as_str()), control)?;
            }
            Expr::QualifiedStar(qualifier) => {
                if let Some(qualified) = source.qualified(qualifier) {
                    columns.extend(qualified.iter().map(|name| name.as_str()), control)?;
                }
            }
            Expr::Column(column) | Expr::QualifiedColumn { column, .. } => {
                columns.push(column, control)?;
            }
            Expr::Func {
                name,
                binding,
                args,
                ..
            } => {
                columns.push_produced(
                    crate::semantics::projection::rule_function_label(
                        name,
                        binding.as_ref(),
                        args.get(1),
                        control,
                    )?,
                    control,
                )?;
            }
            Expr::WindowCall { name, .. } => {
                columns.push_produced(
                    crate::semantics::projection::rule_function_label::<Expr>(
                        name, None, None, control,
                    )?,
                    control,
                )?;
            }
            _ => columns.push("?column?", control)?,
        }
    }
    Ok(columns)
}
fn contains_name(columns: &Columns, candidate: &str) -> bool {
    columns
        .iter()
        .any(|column| column.eq_ignore_ascii_case(candidate))
}
