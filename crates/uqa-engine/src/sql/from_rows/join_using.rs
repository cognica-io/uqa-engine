//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Schema binding and positional output shaping for `PostgreSQL` qualified joins.

use std::collections::{BTreeMap, BTreeSet};

use uqa_execution::{JoinOutput, JoinOutputSource, PhysicalOperator, RowSchema, ScalarExpr};
use uqa_sql::ast::{BinaryOp, JoinKind, JoinUsing};
use uqa_sql::SQLError;

use super::is_score_provenance_column;

#[derive(Debug, Clone)]
pub(in crate::sql) struct ResolvedJoinUsing {
    columns: Vec<ResolvedJoinColumn>,
    alias: Option<String>,
}

#[derive(Debug, Clone)]
struct ResolvedJoinColumn {
    name: String,
    left: usize,
    right: usize,
}

fn visible_columns(schema: &RowSchema) -> Vec<(&str, usize)> {
    schema
        .columns()
        .iter()
        .enumerate()
        .filter_map(|(position, physical)| {
            if is_score_provenance_column(physical) {
                return None;
            }
            let name = physical
                .rsplit_once('.')
                .map_or(physical.as_str(), |(_, column)| column);
            Some((name, position))
        })
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

    let mut columns = Vec::with_capacity(names.len());
    for name in names {
        columns.push(ResolvedJoinColumn {
            left: unique_position(&left_positions, &name, "left")?,
            right: unique_position(&right_positions, &name, "right")?,
            name,
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
        .map(|column| ScalarExpr::Binary {
            op: BinaryOp::Equal,
            lhs: Box::new(ScalarExpr::Column(left.columns()[column.left].clone())),
            rhs: Box::new(ScalarExpr::Column(right.columns()[column.right].clone())),
        })
        .collect::<Vec<_>>();
    match predicates.len() {
        0 => None,
        1 => predicates.pop(),
        _ => Some(ScalarExpr::And(predicates)),
    }
}

fn public_output_name(column: &str) -> String {
    if is_score_provenance_column(column) {
        column.to_string()
    } else {
        column
            .rsplit_once('.')
            .map_or_else(|| column.to_string(), |(_, name)| name.to_string())
    }
}

pub(in crate::sql) fn shape_join_using_output<'a>(
    operator: Box<dyn PhysicalOperator + 'a>,
    kind: JoinKind,
    left: &RowSchema,
    right: &RowSchema,
    using: &ResolvedJoinUsing,
) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
    let right_offset = left.len();
    let mut left_used = vec![false; left.len()];
    let mut right_used = vec![false; right.len()];
    let mut columns = Vec::with_capacity(left.len() + right.len() - using.columns.len());
    let mut merged_sources = Vec::with_capacity(using.columns.len());

    for column in &using.columns {
        left_used[column.left] = true;
        right_used[column.right] = true;
        let source = match kind {
            JoinKind::Inner | JoinKind::Left => JoinOutputSource::Input(column.left),
            JoinKind::Right => JoinOutputSource::Input(right_offset + column.right),
            JoinKind::Full => JoinOutputSource::Coalesce {
                left: column.left,
                right: right_offset + column.right,
            },
            JoinKind::Cross => {
                return Err(SQLError::Internal(
                    "CROSS JOIN cannot carry a USING qualification".into(),
                ));
            }
        };
        columns.push((column.name.clone(), source));
        merged_sources.push((column.name.clone(), source));
    }
    for (position, column) in left.columns().iter().enumerate() {
        if !left_used[position] {
            columns.push((
                public_output_name(column),
                JoinOutputSource::Input(position),
            ));
        }
    }
    for (position, column) in right.columns().iter().enumerate() {
        if !right_used[position] {
            columns.push((
                public_output_name(column),
                JoinOutputSource::Input(right_offset + position),
            ));
        }
    }

    let mut aliases = Vec::new();
    for (position, column) in left.columns().iter().enumerate() {
        if column.contains('.') {
            aliases.push((column.clone(), JoinOutputSource::Input(position)));
        }
    }
    for (position, column) in right.columns().iter().enumerate() {
        if column.contains('.') {
            aliases.push((
                column.clone(),
                JoinOutputSource::Input(right_offset + position),
            ));
        }
    }
    if let Some(alias) = using.alias.as_ref() {
        aliases.extend(
            merged_sources
                .into_iter()
                .map(|(column, source)| (format!("{alias}.{column}"), source)),
        );
    }

    JoinOutput::try_new(operator, columns, aliases)
        .map(|output| Box::new(output) as Box<dyn PhysicalOperator + 'a>)
        .map_err(super::super::select::physical_exec_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_columns_follow_left_input_order() {
        let left = RowSchema::new(vec!["l.b".into(), "l.a".into(), "l.only".into()]);
        let right = RowSchema::new(vec!["r.a".into(), "r.b".into(), "r.other".into()]);
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
        let left = RowSchema::new(vec!["l.id".into(), "other.id".into()]);
        let right = RowSchema::new(vec!["r.id".into()]);
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
