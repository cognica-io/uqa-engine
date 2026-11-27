//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Backslash help and schema/catalog value formatting.

use super::{
    print_result, value_to_display, BTreeMap, ColumnDef, ColumnType, Expr, SQLResult, Value, Write,
};

pub(super) fn print_backslash_help(out: &mut impl Write) {
    let _ = writeln!(out, "Backslash commands:");
    for (command, description) in super::completion::BACKSLASH_COMMANDS {
        let _ = writeln!(out, "  {command:16} {description}");
    }
}

pub(super) fn result_row(entries: Vec<(&str, Value)>) -> BTreeMap<String, Value> {
    entries
        .into_iter()
        .map(|(key, value)| (key.to_string(), value))
        .collect()
}

pub(super) fn usize_count_value(value: usize) -> Value {
    match i64::try_from(value) {
        Ok(value) => Value::Int(value),
        Err(_) => Value::Str(value.to_string()),
    }
}

pub(super) fn u64_count_value(value: u64) -> Value {
    match i64::try_from(value) {
        Ok(value) => Value::Int(value),
        Err(_) => Value::Str(value.to_string()),
    }
}

pub(super) fn sequence_row(
    (name, state): (String, uqa_engine::SequenceState),
) -> BTreeMap<String, Value> {
    result_row(vec![
        ("sequence_name", Value::Str(name)),
        ("start", Value::Int(state.start)),
        ("increment", Value::Int(state.increment)),
        ("current", Value::Int(state.current)),
    ])
}

pub(super) fn print_columns(cols: &[ColumnDef], out: &mut impl Write) {
    let rows = cols
        .iter()
        .map(|col| {
            result_row(vec![
                ("column", Value::Str(col.name.clone())),
                ("type", Value::Str(sql_type_name(&col.ty))),
                ("constraints", Value::Str(column_constraints(col))),
            ])
        })
        .collect();
    print_result(
        &SQLResult::from_rows(
            vec!["column".into(), "type".into(), "constraints".into()],
            rows,
        ),
        out,
    );
}

pub(super) fn optional_value_to_display_value(value: Option<&Value>) -> Result<Value, String> {
    Ok(match value {
        Some(value) => Value::Str(
            value_to_display(Some(value))
                .map_err(|error| super::diagnostics::sql_error_text(&error))?,
        ),
        None => Value::Str(String::new()),
    })
}

pub(super) fn sql_type_name(ty: &ColumnType) -> String {
    match ty {
        ColumnType::Bpchar => "character".into(),
        ColumnType::Numeric {
            precision: Some(precision),
            scale: None,
        } => format!("numeric({precision})"),
        ColumnType::Time => "time".into(),
        ColumnType::Timestamp => "timestamp".into(),
        ColumnType::Domain { schema, name, .. } => format!("{schema}.{name}"),
        ColumnType::Enum(reference) => format!("{}.{}", reference.schema, reference.name),
        ColumnType::Composite(reference) => format!("{}.{}", reference.schema, reference.name),
        ColumnType::DeclaredRange(_) | ColumnType::DeclaredMultirange(_) => ty.display_name(),
        ColumnType::Array(element) => format!("{}[]", sql_type_name(element)),
        _ => ty.sql_name(),
    }
}

pub(super) fn column_constraints(col: &ColumnDef) -> String {
    let mut flags = Vec::new();
    if col.primary_key {
        flags.push("PK".to_string());
    }
    if col.not_null {
        flags.push("NOT NULL".to_string());
    }
    if col.auto_increment.is_some() {
        flags.push("AUTO".to_string());
    }
    if col.unique {
        flags.push("UNIQUE".to_string());
    }
    if let Some(default) = &col.default {
        flags.push(format!("DEFAULT {}", expr_display(default)));
    }
    if let Some(check) = &col.check {
        flags.push(format!("CHECK ({})", expr_display(check)));
    }
    if let Some(reference) = &col.references {
        flags.push(reference.column.as_ref().map_or_else(
            || format!("REFERENCES {}", reference.table),
            |column| format!("REFERENCES {}({column})", reference.table),
        ));
    }
    flags.join(" ")
}

pub(super) fn expr_display(expr: &Expr) -> String {
    format!("{expr:?}")
}

pub(super) fn options_display<'a>(
    options: impl IntoIterator<Item = (&'a String, &'a String)>,
) -> String {
    options
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn foreign_table_options_display(
    options: &uqa_core::memory::OwnedMap<String, String>,
) -> String {
    let mut out = Vec::new();
    if options
        .get("hive_partitioning")
        .is_some_and(|value| value.eq_ignore_ascii_case("true"))
    {
        out.push("hive".to_string());
    }
    out.join(", ")
}

#[cfg(test)]
mod tests {
    use super::sql_type_name;
    use uqa_sql::ColumnType;

    #[test]
    fn expression_pseudo_type_names_preserve_sql_identifier_quoting() {
        use uqa_sql::ast::ExpressionPseudoType::{Any, AnyElement, Unknown};
        for (ty, name) in [
            (Unknown, "unknown"),
            (Any, "\"any\""),
            (AnyElement, "anyelement"),
        ] {
            assert_eq!(sql_type_name(&ColumnType::Pseudo(ty)), name);
        }
    }

    #[test]
    fn schema_type_names_preserve_cli_spellings_and_sql_type_identity() {
        for (ty, name) in [
            (ColumnType::RefCursor, "refcursor"),
            (ColumnType::TsVector, "tsvector"),
            (ColumnType::Bpchar, "character"),
            (ColumnType::Time, "time"),
            (ColumnType::Timestamp, "timestamp"),
            (ColumnType::TimePrecision(2), "time(2) without time zone"),
            (
                ColumnType::Numeric {
                    precision: Some(5),
                    scale: None,
                },
                "numeric(5)",
            ),
        ] {
            assert_eq!(sql_type_name(&ty), name);
            assert_eq!(
                sql_type_name(&ColumnType::Array(Box::new(ty))),
                format!("{name}[]")
            );
        }
    }
}
