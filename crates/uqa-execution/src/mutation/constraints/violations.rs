//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Report a row that fails a NOT NULL, CHECK, partition or view check option constraint as `PostgreSQL` reports it: a row a statement writes with a description of the row that shows what the role may see, an existing row that a table alteration validates without one.

use super::{ConstraintContext, ConstraintStatement};
use crate::catalog::projection::CatalogOutput;
use crate::mutation::errors::dml_storage_error;
use uqa_core::Value;
use uqa_sql::{
    ast::{ColumnType, GeneratedColumnKind},
    result::format_postgres_text,
    semantics::{partition::PartitionRejection, view_mutation::ViewMutationTarget},
    SQLError,
};
use uqa_storage::document_store::Document;

/// The bytes of a value that a description shows; it cuts a longer value at a character boundary and marks the cut with `...` (`maxfieldlen`).
const DESCRIBED_VALUE_BYTES: usize = 64;

/// A NULL in a NOT NULL column: `null value in column "c" of relation "t" violates not-null constraint` for a row a statement writes (`ReportNotNullViolationError`), `column "c" of relation "t" contains null values` for an existing row (`ATRewriteTable`).
pub(super) fn not_null_violation(
    context: ConstraintContext<'_>,
    statement: Option<ConstraintStatement<'_>>,
    table: &str,
    column: &str,
    document: &Document,
) -> SQLError {
    let relation = relation_name(table);
    let Some(statement) = statement else {
        return violation(
            "23502",
            format!("column \"{column}\" of relation \"{relation}\" contains null values"),
            Ok(None),
        );
    };
    violation(
        "23502",
        format!(
            "null value in column \"{column}\" of relation \"{relation}\" violates not-null constraint"
        ),
        failing_row_detail(context, statement, table, document),
    )
}

/// A row that a CHECK constraint rejects: `new row for relation "t" violates check constraint "c"` for a row a statement writes (`ExecConstraints`), `check constraint "c" of relation "t" is violated by some row` for an existing row (`ATRewriteTable`).
pub(super) fn check_violation(
    context: ConstraintContext<'_>,
    statement: Option<ConstraintStatement<'_>>,
    table: &str,
    constraint: &str,
    document: &Document,
) -> SQLError {
    let relation = relation_name(table);
    let Some(statement) = statement else {
        return violation(
            "23514",
            format!(
                "check constraint \"{constraint}\" of relation \"{relation}\" is violated by some row"
            ),
            Ok(None),
        );
    };
    violation(
        "23514",
        format!("new row for relation \"{relation}\" violates check constraint \"{constraint}\""),
        failing_row_detail(context, statement, table, document),
    )
}

/// A row that partition routing or a partition constraint rejects: `new row for relation "p1" violates partition constraint` with a description of the row (`ExecPartitionCheckEmitError`), or `no partition of relation "p" found for row` with the values of its partition key (`ExecFindPartition`).
pub fn partition_rejection_error(
    context: ConstraintContext<'_>,
    statement: ConstraintStatement<'_>,
    rejection: PartitionRejection,
    document: &Document,
) -> SQLError {
    match rejection {
        PartitionRejection::Constraint { relation } => violation(
            "23514",
            format!(
                "new row for relation \"{}\" violates partition constraint",
                relation_name(&relation)
            ),
            failing_row_detail(context, statement, &relation, document),
        ),
        PartitionRejection::NoPartition { relation, keys } => violation(
            "23514",
            format!(
                "no partition of relation \"{}\" found for row",
                relation_name(&relation)
            ),
            partition_key_detail(context, statement, &relation, &keys),
        ),
    }
}

/// A row that a view's check option rejects: `new row violates check option for view "v"` with a description of the row, which `ExecWithCheckOptions` builds as it does for a CHECK constraint.
pub fn view_check_violation(
    context: ConstraintContext<'_>,
    statement: ConstraintStatement<'_>,
    view: &str,
    table: &str,
    document: &Document,
) -> SQLError {
    violation(
        "44000",
        format!(
            "new row violates check option for view \"{}\"",
            relation_name(view)
        ),
        failing_row_detail(context, statement, table, document),
    )
}

/// A row that a view's check option rejects once an `INSTEAD OF` trigger of `target`, the view the statement was rewritten to, returned it: `ExecWithCheckOptions` describes the row in the columns of `target`, the statement's result relation, showing a role that may not read `target` the columns it may read or `supplied` names.
pub fn trigger_view_check_violation(
    context: ConstraintContext<'_>,
    view: &str,
    target: &ViewMutationTarget,
    supplied: &[String],
    values: &[Value],
) -> SQLError {
    violation(
        "44000",
        format!(
            "new row violates check option for view \"{}\"",
            relation_name(view)
        ),
        view_row_detail(context, target, supplied, values),
    )
}

/// The violation, or the error that describing the row raised instead.
fn violation(
    sqlstate: &str,
    message: String,
    detail: Result<Option<String>, SQLError>,
) -> SQLError {
    match detail {
        Ok(detail) => SQLError::Diagnostic {
            sqlstate: sqlstate.into(),
            message,
            detail,
            hint: None,
        },
        Err(error) => error,
    }
}

/// `Failing row contains (1, x).` (`ExecBuildSlotValueDescription`). The row is described in the columns of the relation the statement names when it stores the row in one of that relation's partitions or inheritance children. A role that may read that relation sees every column; any other role sees the columns it may read or the statement supplies, as `Failing row contains (a, b) = (1, x).`, and nothing when there is no such column.
fn failing_row_detail(
    context: ConstraintContext<'_>,
    statement: ConstraintStatement<'_>,
    table: &str,
    document: &Document,
) -> Result<Option<String>, SQLError> {
    let relation = described_relation(context, statement, table)?;
    let columns = context
        .catalog
        .try_describe_table(&relation)
        .map_err(|error| dml_storage_error("constraint violation", error))?
        .ok_or_else(|| SQLError::UnknownTable(relation.clone()))?;
    let diagnostics = context.diagnostics.diagnostic_context();
    let visible = diagnostics.authorization.row_description_columns(
        &relation,
        statement.columns,
        statement.referential_action,
    )?;
    let output = CatalogOutput(diagnostics.catalog);
    describe_row(
        visible.as_deref(),
        columns.iter().map(|column| {
            let virtual_column = column
                .generated
                .as_ref()
                .is_some_and(|generated| generated.kind == GeneratedColumnKind::Virtual);
            DescribedColumn {
                name: &column.name,
                value: if virtual_column {
                    DescribedValue::Virtual
                } else {
                    DescribedValue::Stored(document.get(&column.name))
                },
                ty: Some(&column.ty),
            }
        }),
        &output,
    )
}

/// The description of a row an `INSTEAD OF` trigger of `target` returned, in the columns of `target`.
fn view_row_detail(
    context: ConstraintContext<'_>,
    target: &ViewMutationTarget,
    supplied: &[String],
    values: &[Value],
) -> Result<Option<String>, SQLError> {
    let diagnostics = context.diagnostics.diagnostic_context();
    let visible = diagnostics.authorization.view_row_description_columns(
        &target.definition,
        &target.columns,
        supplied,
    )?;
    let output = CatalogOutput(diagnostics.catalog);
    describe_row(
        visible.as_deref(),
        target
            .columns
            .iter()
            .zip(&target.types)
            .zip(values)
            .map(|((name, ty), value)| DescribedColumn {
                name,
                value: DescribedValue::Stored(Some(value)),
                ty: ty.as_ref(),
            }),
        &output,
    )
}

/// One column of a described row.
struct DescribedColumn<'a> {
    name: &'a str,
    value: DescribedValue<'a>,
    /// The column's type, whose output function shows the value; a view column whose type is unknown shows the value as it is.
    ty: Option<&'a ColumnType>,
}

enum DescribedValue<'a> {
    /// A stored value; a missing value is NULL.
    Stored(Option<&'a Value>),
    /// A virtual generated column, whose value is not computed for the description.
    Virtual,
}

/// `Failing row contains (1, x).`, or `Failing row contains (a, b) = (1, x).` when `visible` names the columns the role may see, or nothing when it may see none.
fn describe_row<'a>(
    visible: Option<&[String]>,
    columns: impl Iterator<Item = DescribedColumn<'a>>,
    output: &CatalogOutput<'_>,
) -> Result<Option<String>, SQLError> {
    let mut names = Vec::new();
    let mut values = Vec::new();
    for column in columns {
        if let Some(visible) = visible {
            if !visible.iter().any(|name| name == column.name) {
                continue;
            }
            names.push(column.name);
        }
        values.push(match column.value {
            DescribedValue::Virtual => "virtual".to_string(),
            DescribedValue::Stored(None | Some(Value::Null)) => "null".to_string(),
            DescribedValue::Stored(Some(value)) => clip(match column.ty {
                Some(ty) => format_postgres_text(value, ty, Some(output))?,
                None => uqa_sql::expr::value_to_string(value)?,
            }),
        });
    }
    Ok(match visible {
        None => Some(format!("Failing row contains ({}).", values.join(", "))),
        Some(_) if names.is_empty() => None,
        Some(_) => Some(format!(
            "Failing row contains ({}) = ({}).",
            names.join(", "),
            values.join(", ")
        )),
    })
}

/// `Partition key of the failing row contains (k) = (1).` (`ExecBuildSlotPartitionKeyDescription`): the keys of the partition key of `relation` and the row's values for them, when the role may read the table or each key is a column it may read.
fn partition_key_detail(
    context: ConstraintContext<'_>,
    statement: ConstraintStatement<'_>,
    relation: &str,
    keys: &[Value],
) -> Result<Option<String>, SQLError> {
    let hierarchy = context
        .partitions
        .catalog
        .try_table_hierarchy(relation)
        .map_err(|error| SQLError::Internal(format!("read partition metadata: {error}")))?;
    let spec = hierarchy.partition_spec.as_ref().ok_or_else(|| {
        SQLError::Internal(format!(
            "partitioned table `{relation}` has no partition key"
        ))
    })?;
    let diagnostics = context.diagnostics.diagnostic_context();
    if !diagnostics.authorization.can_view_partition_key(
        relation,
        &spec.keys,
        statement.referential_action,
    )? {
        return Ok(None);
    }
    let catalog = diagnostics.catalog.catalog_read_view();
    let resolution = diagnostics
        .catalog
        .session_execution_view()
        .relation_name_resolution();
    let names =
        crate::catalog::projection::partition_key_columns(&catalog, &resolution, spec, true)?;
    let types = crate::catalog::projection::partition_key_types_for_table(
        &diagnostics.catalog,
        &catalog,
        &resolution,
        relation,
    )?;
    let output = CatalogOutput(diagnostics.catalog);
    let values = keys
        .iter()
        .zip(&types)
        .map(|(value, ty)| match value {
            Value::Null => Ok("null".to_string()),
            value => format_postgres_text(value, ty, Some(&output)).map(clip),
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    Ok(Some(format!(
        "Partition key of the failing row contains ({names}) = ({}).",
        values.join(", ")
    )))
}

/// The relation whose columns describe a row stored in `table`: the relation the statement names when `table` is that relation or one of its partitions or inheritance children (`ri_RootResultRelInfo`), and `table` otherwise.
fn described_relation(
    context: ConstraintContext<'_>,
    statement: ConstraintStatement<'_>,
    table: &str,
) -> Result<String, SQLError> {
    if statement.relation == table
        || context
            .catalog
            .hierarchy_scan_tables(statement.relation, true)?
            .iter()
            .any(|descendant| descendant == table)
    {
        return Ok(statement.relation.to_string());
    }
    Ok(table.to_string())
}

/// The relation's own name, which `PostgreSQL`'s violations print without its schema.
fn relation_name(table: &str) -> String {
    uqa_core::RelationIdentity::from_legacy_name(table)
        .map_or_else(|_| table.to_string(), |identity| identity.name)
}

/// The value cut to [`DESCRIBED_VALUE_BYTES`] bytes at a character boundary, with `...` marking the cut (`pg_mbcliplen`).
fn clip(mut text: String) -> String {
    if text.len() <= DESCRIBED_VALUE_BYTES {
        return text;
    }
    let mut end = DESCRIBED_VALUE_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text.push_str("...");
    text
}

#[cfg(test)]
mod tests {
    use super::clip;

    #[test]
    fn a_described_value_is_cut_at_a_character_boundary_after_64_bytes() {
        assert_eq!(clip("a".repeat(64)), "a".repeat(64));
        assert_eq!(clip("a".repeat(65)), format!("{}...", "a".repeat(64)));
        // Each Greek letter takes two bytes, so 64 bytes hold 32 of them.
        let greek = "\u{3ba}\u{3cc}\u{3c3}\u{3bc}\u{3b5}".repeat(10);
        let clipped = clip(greek.clone());
        assert_eq!(
            clipped,
            format!("{}...", greek.chars().take(32).collect::<String>())
        );
        // A three-byte character that straddles the limit is left out whole.
        let wide = format!("{}\u{20ac}", "a".repeat(63));
        assert_eq!(clip(wide), format!("{}...", "a".repeat(63)));
    }
}
