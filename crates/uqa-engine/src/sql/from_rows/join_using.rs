//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Schema binding and positional output shaping for `PostgreSQL` qualified joins.

use std::collections::{BTreeMap, BTreeSet};

use uqa_execution::{
    ColumnIdentity, JoinOutput, JoinOutputSource, PhysicalOperator, RowSchema, ScalarExpr,
};
use uqa_sql::ast::{BinaryOp, ColumnType, JoinKind, JoinUsing};
use uqa_sql::SQLError;

#[derive(Debug, Clone)]
pub(in crate::sql) struct ResolvedJoinUsing {
    columns: Vec<ResolvedJoinColumn>,
    alias: Option<String>,
}

type JoinUsingLayout = (
    Vec<(String, ColumnIdentity, JoinOutputSource)>,
    Vec<(ColumnIdentity, JoinOutputSource)>,
);

#[derive(Debug, Clone)]
struct ResolvedJoinColumn {
    name: String,
    left: usize,
    right: usize,
    comparison_type: Option<ColumnType>,
    output_type: Option<ColumnType>,
}

fn visible_columns(schema: &RowSchema) -> Vec<(&str, usize)> {
    schema
        .columns()
        .iter()
        .enumerate()
        .filter_map(|(position, _)| Some((schema.public_name(position)?, position)))
        .collect()
}

fn positions_by_name<'a>(columns: &'a [(&'a str, usize)]) -> BTreeMap<&'a str, Vec<usize>> {
    let mut positions = BTreeMap::<&str, Vec<usize>>::new();
    for (name, position) in columns {
        positions.entry(name).or_default().push(*position);
    }
    positions
}

fn missing_using_column(column: &str, side: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "42703".into(),
        message: format!(
            "column \"{column}\" specified in USING clause does not exist in {side} table"
        ),
    }
}

fn ambiguous_using_column(column: &str, side: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "42702".into(),
        message: format!("common column name \"{column}\" appears more than once in {side} table"),
    }
}

fn unique_position(
    positions: &BTreeMap<&str, Vec<usize>>,
    column: &str,
    side: &str,
) -> Result<usize, SQLError> {
    match positions.get(column).map(Vec::as_slice) {
        None | Some([]) => Err(missing_using_column(column, side)),
        Some([position]) => Ok(*position),
        Some(_) => Err(ambiguous_using_column(column, side)),
    }
}

pub(in crate::sql) fn resolve_join_using(
    explicit: Option<&JoinUsing>,
    natural: bool,
    left: &RowSchema,
    right: &RowSchema,
) -> Result<Option<ResolvedJoinUsing>, SQLError> {
    if explicit.is_none() && !natural {
        return Ok(None);
    }
    if explicit.is_some() && natural {
        return Err(SQLError::Internal(
            "JOIN cannot be both USING-qualified and NATURAL".into(),
        ));
    }

    let left_columns = visible_columns(left);
    let right_columns = visible_columns(right);
    let left_positions = positions_by_name(&left_columns);
    let right_positions = positions_by_name(&right_columns);
    let (names, alias) = if let Some(using) = explicit {
        let mut seen = BTreeSet::new();
        for name in &using.columns {
            if !seen.insert(name.as_str()) {
                return Err(SQLError::Routine {
                    sqlstate: "42701".into(),
                    message: format!(
                        "column name \"{name}\" appears more than once in USING clause"
                    ),
                });
            }
        }
        (using.columns.clone(), using.alias.clone())
    } else {
        let mut names = Vec::new();
        let mut seen = BTreeSet::new();
        for (name, _) in &left_columns {
            if right_positions.contains_key(name) && seen.insert(*name) {
                names.push((*name).to_string());
            }
        }
        (names, None)
    };

    if let Some(alias) = alias.as_deref() {
        if left.has_qualifier(alias) || right.has_qualifier(alias) {
            return Err(SQLError::Routine {
                sqlstate: "42712".into(),
                message: format!("table name \"{alias}\" specified more than once"),
            });
        }
    }

    let mut columns = Vec::with_capacity(names.len());
    for name in names {
        let left_position = unique_position(&left_positions, &name, "left")?;
        let right_position = unique_position(&right_positions, &name, "right")?;
        let (comparison_type, output_type) = match (
            left.column_type(left_position),
            right.column_type(right_position),
        ) {
            (Some(left_type), Some(right_type)) => (
                Some(uqa_execution::equality_operand_type(left_type, right_type)?),
                Some(uqa_execution::common_type(left_type, right_type)?),
            ),
            _ => (None, None),
        };
        columns.push(ResolvedJoinColumn {
            left: left_position,
            right: right_position,
            name,
            comparison_type,
            output_type,
        });
    }
    Ok(Some(ResolvedJoinUsing { columns, alias }))
}

pub(in crate::sql) fn join_using_predicate(
    using: &ResolvedJoinUsing,
    left: &RowSchema,
    right: &RowSchema,
) -> Option<ScalarExpr> {
    let mut predicates = using
        .columns
        .iter()
        .map(|column| {
            let lhs = coerced_join_column(
                scalar_column(left, column.left),
                left.column_type(column.left),
                column.comparison_type.as_ref(),
            );
            let rhs = coerced_join_column(
                scalar_column(right, column.right),
                right.column_type(column.right),
                column.comparison_type.as_ref(),
            );
            ScalarExpr::Binary {
                op: BinaryOp::Equal,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            }
        })
        .collect::<Vec<_>>();
    match predicates.len() {
        0 => None,
        1 => predicates.pop(),
        _ => Some(ScalarExpr::And(predicates)),
    }
}

fn scalar_column(schema: &RowSchema, position: usize) -> ScalarExpr {
    let identity = &schema.identities()[position];
    identity.qualifier().map_or_else(
        || ScalarExpr::Column(identity.column().to_string()),
        |qualifier| ScalarExpr::qualified_column(qualifier, identity.column()),
    )
}

fn coerced_join_column(
    expression: ScalarExpr,
    source: Option<&ColumnType>,
    target: Option<&ColumnType>,
) -> ScalarExpr {
    match (source, target) {
        (Some(source), Some(target)) if source != target => ScalarExpr::Cast {
            expr: Box::new(expression),
            ty: target.sql_name(),
        },
        _ => expression,
    }
}

pub(in crate::sql) fn shape_join_using_output<'a>(
    operator: Box<dyn PhysicalOperator + 'a>,
    kind: JoinKind,
    left: &RowSchema,
    right: &RowSchema,
    using: &ResolvedJoinUsing,
) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
    let (columns, aliases) = join_using_layout(kind, left, right, using)?;
    JoinOutput::try_new(operator, columns, aliases)
        .map(|output| Box::new(output) as Box<dyn PhysicalOperator + 'a>)
        .map_err(super::super::select::physical_exec_error)
}

/// Bind the visible and hidden output identities of a qualified join without
/// constructing or executing the join. Static source-schema binding and the
/// physical operator share this layout so an empty or correlated right side
/// cannot lose its declared types.
pub(in crate::sql) fn join_using_output_schema(
    kind: JoinKind,
    left: &RowSchema,
    right: &RowSchema,
    using: &ResolvedJoinUsing,
) -> Result<RowSchema, SQLError> {
    let input = RowSchema::join(left, right, std::iter::empty());
    let (columns, aliases) = join_using_layout(kind, left, right, using)?;
    JoinOutput::try_schema(&input, &columns, &aliases)
        .map_err(super::super::select::physical_exec_error)
}

/// Add a parenthesized JOIN alias to the input-side schemas for null-rejection analysis. Merged FULL JOIN columns belong to neither side exclusively because their value is a coalesce of both inputs.
pub(in crate::sql) fn join_alias_input_schemas(
    kind: JoinKind,
    left: &RowSchema,
    right: &RowSchema,
    using: Option<&ResolvedJoinUsing>,
    alias: &str,
    column_aliases: &[String],
) -> Result<(RowSchema, RowSchema), SQLError> {
    #[derive(Clone, Copy)]
    enum Owner {
        Left(usize),
        Right(usize),
        Neither,
    }

    let mut output = Vec::<(String, Owner)>::new();
    if let Some(using) = using {
        let mut left_used = vec![false; left.len()];
        let mut right_used = vec![false; right.len()];
        for column in &using.columns {
            left_used[column.left] = true;
            right_used[column.right] = true;
            let owner = match kind {
                JoinKind::Inner | JoinKind::Left => Owner::Left(column.left),
                JoinKind::Right => Owner::Right(column.right),
                JoinKind::Full => Owner::Neither,
                JoinKind::Cross => {
                    return Err(SQLError::Internal(
                        "CROSS JOIN cannot carry a USING qualification".into(),
                    ));
                }
            };
            output.push((column.name.clone(), owner));
        }
        output.extend(
            left.columns()
                .iter()
                .enumerate()
                .filter(|(position, _)| !left_used[*position])
                .map(|(position, column)| {
                    (
                        left.public_name(position).unwrap_or(column).to_string(),
                        Owner::Left(position),
                    )
                }),
        );
        output.extend(
            right
                .columns()
                .iter()
                .enumerate()
                .filter(|(position, _)| !right_used[*position])
                .map(|(position, column)| {
                    (
                        right.public_name(position).unwrap_or(column).to_string(),
                        Owner::Right(position),
                    )
                }),
        );
    } else {
        output.extend(left.columns().iter().enumerate().map(|(position, column)| {
            (
                left.public_name(position).unwrap_or(column).to_string(),
                Owner::Left(position),
            )
        }));
        output.extend(
            right
                .columns()
                .iter()
                .enumerate()
                .map(|(position, column)| {
                    (
                        right.public_name(position).unwrap_or(column).to_string(),
                        Owner::Right(position),
                    )
                }),
        );
    }

    if column_aliases.len() > output.len() {
        return Err(SQLError::Routine {
            sqlstate: "42P10".into(),
            message: format!(
                "join expression \"{alias}\" has {} columns available but {} columns specified",
                output.len(),
                column_aliases.len()
            ),
        });
    }
    for (position, column_alias) in column_aliases.iter().enumerate() {
        output[position].0.clone_from(column_alias);
    }

    let mut counts = BTreeMap::<String, usize>::new();
    for (column, _) in &output {
        *counts.entry(column.clone()).or_default() += 1;
    }
    let mut left_aliases = Vec::new();
    let mut right_aliases = Vec::new();
    for (column, owner) in output {
        if counts.get(&column) != Some(&1) {
            continue;
        }
        let identity = ColumnIdentity::qualified(alias, column);
        match owner {
            Owner::Left(position) => left_aliases.push((identity, position)),
            Owner::Right(position) => right_aliases.push((identity, position)),
            Owner::Neither => {}
        }
    }
    Ok((
        RowSchema::with_identity_aliases(left, &left_aliases),
        RowSchema::with_identity_aliases(right, &right_aliases),
    ))
}

fn join_using_layout(
    kind: JoinKind,
    left: &RowSchema,
    right: &RowSchema,
    using: &ResolvedJoinUsing,
) -> Result<JoinUsingLayout, SQLError> {
    let right_offset = left.len();
    let mut left_used = vec![false; left.len()];
    let mut right_used = vec![false; right.len()];
    let mut columns = Vec::with_capacity(left.len() + right.len() - using.columns.len());
    let mut merged_sources = Vec::with_capacity(using.columns.len());

    for column in &using.columns {
        left_used[column.left] = true;
        right_used[column.right] = true;
        let source = match kind {
            JoinKind::Inner | JoinKind::Left => coerced_output_source(
                column.left,
                left.column_type(column.left),
                column.output_type.as_ref(),
            ),
            JoinKind::Right => coerced_output_source(
                right_offset + column.right,
                right.column_type(column.right),
                column.output_type.as_ref(),
            ),
            JoinKind::Full => {
                column
                    .output_type
                    .as_ref()
                    .map_or(JoinOutputSource::Input(column.left), |ty| {
                        JoinOutputSource::Coalesce {
                            left: column.left,
                            right: right_offset + column.right,
                            ty: ty.clone(),
                        }
                    })
            }
            JoinKind::Cross => {
                return Err(SQLError::Internal(
                    "CROSS JOIN cannot carry a USING qualification".into(),
                ));
            }
        };
        columns.push((
            column.name.clone(),
            ColumnIdentity::unqualified(&column.name),
            source.clone(),
        ));
        merged_sources.push((column.name.clone(), source));
    }
    for (position, column) in left.columns().iter().enumerate() {
        if !left_used[position] {
            columns.push((
                left.public_name(position).unwrap_or(column).to_string(),
                left.identities()[position].clone(),
                JoinOutputSource::Input(position),
            ));
        }
    }
    for (position, column) in right.columns().iter().enumerate() {
        if !right_used[position] {
            columns.push((
                right.public_name(position).unwrap_or(column).to_string(),
                right.identities()[position].clone(),
                JoinOutputSource::Input(right_offset + position),
            ));
        }
    }

    let mut aliases = Vec::new();
    for (position, identity) in left.identities().iter().enumerate() {
        if identity.qualifier().is_some() {
            aliases.push((identity.clone(), JoinOutputSource::Input(position)));
        }
    }
    for (position, identity) in right.identities().iter().enumerate() {
        if identity.qualifier().is_some() {
            aliases.push((
                identity.clone(),
                JoinOutputSource::Input(right_offset + position),
            ));
        }
    }
    if let Some(alias) = using.alias.as_ref() {
        aliases.extend(
            merged_sources
                .into_iter()
                .map(|(column, source)| (ColumnIdentity::qualified(alias, column), source)),
        );
    }

    Ok((columns, aliases))
}

fn coerced_output_source(
    input: usize,
    source: Option<&ColumnType>,
    target: Option<&ColumnType>,
) -> JoinOutputSource {
    match (source, target) {
        (Some(source), Some(target)) if source != target => JoinOutputSource::Cast {
            input,
            ty: target.clone(),
        },
        _ => JoinOutputSource::Input(input),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_columns_follow_left_input_order() {
        let left = RowSchema::with_qualified_types(
            "l",
            vec!["b".into(), "a".into(), "only".into()],
            vec![None; 3],
        );
        let right = RowSchema::with_qualified_types(
            "r",
            vec!["a".into(), "b".into(), "other".into()],
            vec![None; 3],
        );
        let resolved = resolve_join_using(None, true, &left, &right)
            .unwrap()
            .unwrap();
        assert_eq!(
            resolved
                .columns
                .iter()
                .map(|column| column.name.as_str())
                .collect::<Vec<_>>(),
            ["b", "a"]
        );
    }

    #[test]
    fn using_requires_one_column_on_each_side() {
        let left = RowSchema::with_identities(
            vec!["id".into(), "id".into()],
            vec![
                ColumnIdentity::qualified("l", "id"),
                ColumnIdentity::qualified("other", "id"),
            ],
            vec![None; 2],
        );
        let right = RowSchema::with_qualified_types("r", vec!["id".into()], vec![None]);
        let error = resolve_join_using(
            Some(&JoinUsing {
                columns: vec!["id".into()],
                alias: None,
            }),
            false,
            &left,
            &right,
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("42702"));
    }
}
