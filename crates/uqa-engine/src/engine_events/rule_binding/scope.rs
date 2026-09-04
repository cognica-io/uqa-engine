//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL source-name and shadowing scopes for rewrite-rule binding.

use std::collections::{BTreeMap, BTreeSet};

use uqa_sql::ast::{Expr, FromClause, SelectStmt};
use uqa_sql::SQLError;

use super::RuleBindingContext;

/// Names introduced by a nested query that take precedence over rule OLD/NEW rows.
#[derive(Clone, Default)]
pub(super) struct RuleBindingScope {
    qualifiers: BTreeSet<String>,
    columns: BTreeSet<String>,
}

#[derive(Default)]
struct RuleSourceNames {
    columns: Vec<String>,
    qualified_columns: BTreeMap<String, Vec<String>>,
}

impl RuleSourceNames {
    fn insert_qualifier(&mut self, qualifier: &str, columns: &[String]) {
        self.qualified_columns
            .insert(qualifier.to_ascii_lowercase(), columns.to_vec());
    }

    fn qualified(&self, qualifier: &str) -> Option<&[String]> {
        self.qualified_columns
            .get(&qualifier.to_ascii_lowercase())
            .map(Vec::as_slice)
    }
}

impl RuleBindingScope {
    pub(super) fn from_qualifiers(qualifiers: &BTreeSet<String>) -> Self {
        Self {
            qualifiers: qualifiers
                .iter()
                .map(|qualifier| qualifier.to_ascii_lowercase())
                .collect(),
            columns: BTreeSet::new(),
        }
    }

    pub(super) fn insert_qualifier(&mut self, qualifier: &str) {
        self.qualifiers.insert(qualifier.to_ascii_lowercase());
    }

    fn insert_column(&mut self, column: &str) {
        self.columns.insert(column.to_ascii_lowercase());
    }

    pub(super) fn qualifier_is_shadowed(&self, qualifier: &str) -> bool {
        self.qualifiers.contains(&qualifier.to_ascii_lowercase())
    }

    pub(super) fn column_is_shadowed(&self, column: &str) -> bool {
        self.columns.contains(&column.to_ascii_lowercase())
    }
}

pub(super) fn collect_visible_scope(
    from: &FromClause,
    context: &RuleBindingContext<'_>,
    output: &mut RuleBindingScope,
) -> Result<(), SQLError> {
    let names = source_names(from, context)?;
    for qualifier in names.qualified_columns.keys() {
        output.insert_qualifier(qualifier);
    }
    for column in names.columns {
        output.insert_column(&column);
    }
    Ok(())
}

fn source_names(
    from: &FromClause,
    context: &RuleBindingContext<'_>,
) -> Result<RuleSourceNames, SQLError> {
    match from {
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
        } => Ok(aliased_source_names(
            (1..=rows.first().map_or(0, Vec::len))
                .map(|position| format!("column{position}"))
                .collect(),
            alias.as_deref(),
            column_aliases,
        )),
        FromClause::Subquery {
            body,
            alias,
            column_aliases,
        } => Ok(aliased_source_names(
            select_output_columns(body, context)?,
            alias.as_deref(),
            column_aliases,
        )),
        FromClause::Function {
            output_name,
            alias,
            column_aliases,
            ordinality,
            ..
        } => Ok(function_source_names(
            output_name,
            alias.as_deref(),
            column_aliases,
            *ordinality,
        )),
        FromClause::FunctionGroup {
            functions,
            alias,
            column_aliases,
            ordinality,
        } => Ok(function_group_source_names(
            functions,
            alias.as_deref(),
            column_aliases,
            *ordinality,
        )),
    }
}

fn table_source_names(
    context: &RuleBindingContext<'_>,
    name: &str,
    qualifier: &str,
    alias: Option<&str>,
    column_aliases: &[String],
) -> Result<RuleSourceNames, SQLError> {
    let mut columns = context.relation_columns(name)?;
    apply_positional_aliases(&mut columns, column_aliases);
    let mut names = RuleSourceNames {
        columns: columns.clone(),
        ..RuleSourceNames::default()
    };
    if let Some(alias) = alias {
        names.insert_qualifier(alias, &columns);
    } else {
        names.insert_qualifier(qualifier, &columns);
        names.insert_qualifier(name, &columns);
        if let Some((_, local)) = name.rsplit_once('.') {
            names.insert_qualifier(local.trim_matches('"'), &columns);
        }
    }
    Ok(names)
}

fn join_source_names(
    context: &RuleBindingContext<'_>,
    left: &FromClause,
    right: &FromClause,
    alias: Option<&str>,
    column_aliases: &[String],
    using: Option<&uqa_sql::ast::JoinUsing>,
    natural: bool,
) -> Result<RuleSourceNames, SQLError> {
    let left = source_names(left, context)?;
    let right = source_names(right, context)?;
    let merged = if natural {
        left.columns
            .iter()
            .filter(|column| contains_name(&right.columns, column))
            .cloned()
            .collect::<Vec<_>>()
    } else {
        using.map_or_else(Vec::new, |using| using.columns.clone())
    };
    let mut columns = merged.clone();
    columns.extend(
        left.columns
            .iter()
            .filter(|column| !contains_name(&merged, column))
            .cloned(),
    );
    columns.extend(
        right
            .columns
            .iter()
            .filter(|column| !contains_name(&merged, column))
            .cloned(),
    );
    apply_positional_aliases(&mut columns, column_aliases);
    let mut names = RuleSourceNames {
        columns: columns.clone(),
        ..RuleSourceNames::default()
    };
    if let Some(alias) = alias {
        names.insert_qualifier(alias, &columns);
    } else {
        names.qualified_columns.extend(left.qualified_columns);
        names.qualified_columns.extend(right.qualified_columns);
    }
    if let Some(alias) = using.and_then(|using| using.alias.as_deref()) {
        names.insert_qualifier(alias, &merged);
    }
    Ok(names)
}

fn aliased_source_names(
    mut columns: Vec<String>,
    alias: Option<&str>,
    column_aliases: &[String],
) -> RuleSourceNames {
    apply_positional_aliases(&mut columns, column_aliases);
    let mut names = RuleSourceNames {
        columns: columns.clone(),
        ..RuleSourceNames::default()
    };
    if let Some(alias) = alias {
        names.insert_qualifier(alias, &columns);
    }
    names
}

fn function_source_names(
    output_name: &str,
    alias: Option<&str>,
    column_aliases: &[String],
    ordinality: bool,
) -> RuleSourceNames {
    let mut columns = vec![output_name.to_string()];
    apply_positional_aliases(&mut columns, column_aliases);
    if ordinality {
        columns.push("ordinality".into());
    }
    let mut names = RuleSourceNames {
        columns: columns.clone(),
        ..RuleSourceNames::default()
    };
    names.insert_qualifier(alias.unwrap_or(output_name), &columns);
    names
}

fn function_group_source_names(
    functions: &[uqa_sql::ast::TableFunction],
    alias: Option<&str>,
    column_aliases: &[String],
    ordinality: bool,
) -> RuleSourceNames {
    let mut columns = functions
        .iter()
        .flat_map(function_output_columns)
        .collect::<Vec<_>>();
    apply_positional_aliases(&mut columns, column_aliases);
    if ordinality {
        columns.push("ordinality".into());
    }
    let mut names = RuleSourceNames {
        columns: columns.clone(),
        ..RuleSourceNames::default()
    };
    if let Some(alias) = alias {
        names.insert_qualifier(alias, &columns);
    } else {
        for function in functions {
            names.insert_qualifier(&function.output_name, &function_output_columns(function));
        }
    }
    names
}

fn function_output_columns(function: &uqa_sql::ast::TableFunction) -> Vec<String> {
    if function.column_aliases.is_empty() {
        vec![function.output_name.clone()]
    } else {
        function.column_aliases.clone()
    }
}

pub(super) fn select_output_columns(
    select: &SelectStmt,
    context: &RuleBindingContext<'_>,
) -> Result<Vec<String>, SQLError> {
    let context = context.with_ctes(&select.with)?;
    if let Some(left) = select.set_op.as_ref().and_then(|set| set.left.as_deref()) {
        return select_output_columns(left, &context);
    }
    if select.projections.is_empty() && !select.values.is_empty() {
        return Ok((1..=select.values.first().map_or(0, Vec::len))
            .map(|position| format!("column{position}"))
            .collect());
    }
    let source = select
        .from
        .as_ref()
        .map(|source| source_names(source, &context))
        .transpose()?
        .unwrap_or_default();
    let mut columns = Vec::new();
    for projection in &select.projections {
        if let Some(alias) = &projection.alias {
            columns.push(alias.clone());
            continue;
        }
        match &projection.expr {
            Expr::Star => columns.extend(source.columns.iter().cloned()),
            Expr::QualifiedStar(qualifier) => {
                if let Some(qualified) = source.qualified(qualifier) {
                    columns.extend(qualified.iter().cloned());
                }
            }
            Expr::Column(column) | Expr::QualifiedColumn { column, .. } => {
                columns.push(column.clone());
            }
            Expr::Func { name, .. } => columns.push(
                name.rsplit_once('.')
                    .map_or(name.as_str(), |(_, local)| local)
                    .to_string(),
            ),
            _ => columns.push("?column?".into()),
        }
    }
    Ok(columns)
}

pub(super) fn apply_positional_aliases(columns: &mut Vec<String>, aliases: &[String]) {
    if columns.is_empty() {
        columns.extend_from_slice(aliases);
        return;
    }
    for (position, alias) in aliases.iter().enumerate() {
        if let Some(column) = columns.get_mut(position) {
            column.clone_from(alias);
        }
    }
}

fn contains_name(columns: &[String], candidate: &str) -> bool {
    columns
        .iter()
        .any(|column| column.eq_ignore_ascii_case(candidate))
}
