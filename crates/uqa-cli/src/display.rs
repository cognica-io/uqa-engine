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
    let _ = writeln!(out, "  \\dt             List tables");
    let _ = writeln!(out, "  \\d  <table>     Describe table schema");
    let _ = writeln!(out, "  \\di             List inverted-index fields");
    let _ = writeln!(out, "  \\ds [sequence]  List sequences");
    let _ = writeln!(out, "  \\dF             List foreign tables");
    let _ = writeln!(out, "  \\dS             List foreign servers");
    let _ = writeln!(out, "  \\dg             List named graphs");
    let _ = writeln!(out, "  \\stats <table>  Show column statistics");
    let _ = writeln!(out, "  \\x              Toggle expanded display");
    let _ = writeln!(out, "  \\o  [file]      Redirect output to file");
    let _ = writeln!(out, "  \\timing         Toggle query timing");
    let _ = writeln!(out, "  \\reset          Reset engine");
    let _ = writeln!(out, "  \\run <file>     Execute SQL from a file");
    let _ = writeln!(
        out,
        "  \\open <path>    Switch to persistent storage (prompts for the key of encrypted databases)"
    );
    let _ = writeln!(
        out,
        "  \\new            Switch to a fresh in-memory database"
    );
    let _ = writeln!(out, "  \\where          Show current database");
    let _ = writeln!(out, "  \\q              Quit");
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

pub(super) fn optional_value_to_display_value(value: Option<&Value>) -> Value {
    match value {
        Some(value) => Value::Str(value_to_display(Some(value))),
        None => Value::Str(String::new()),
    }
}

pub(super) fn sql_type_name(ty: &ColumnType) -> String {
    match ty {
        ColumnType::SmallInteger => "smallint".into(),
        ColumnType::Integer => "integer".into(),
        ColumnType::BigInteger => "bigint".into(),
        ColumnType::Oid => "oid".into(),
        ColumnType::Xid => "xid".into(),
        ColumnType::Boolean => "boolean".into(),
        ColumnType::Text => "text".into(),
        ColumnType::Name => "name".into(),
        ColumnType::Uuid => "uuid".into(),
        ColumnType::Varchar(Some(length)) => format!("character varying({length})"),
        ColumnType::Varchar(None) => "character varying".into(),
        ColumnType::Bpchar => "character".into(),
        ColumnType::Character(length) => format!("character({length})"),
        ColumnType::Real => "real".into(),
        ColumnType::DoublePrecision => "double precision".into(),
        ColumnType::Numeric { precision, scale } => match (precision, scale) {
            (Some(p), Some(s)) => format!("numeric({p},{s})"),
            (Some(p), None) => format!("numeric({p})"),
            _ => "numeric".into(),
        },
        ColumnType::Json => "json".into(),
        ColumnType::JsonB => "jsonb".into(),
        ColumnType::Bytea => "bytea".into(),
        ColumnType::InternalChar => r#""char""#.into(),
        ColumnType::Regproc => "regproc".into(),
        ColumnType::Regclass => "regclass".into(),
        ColumnType::Regnamespace => "regnamespace".into(),
        ColumnType::Regtype => "regtype".into(),
        ColumnType::PgNodeTree => "pg_node_tree".into(),
        ColumnType::AclItem => "aclitem".into(),
        ColumnType::Int2Vector => "int2vector".into(),
        ColumnType::OidVector => "oidvector".into(),
        ColumnType::AnyArray => "anyarray".into(),
        ColumnType::Array(element) => format!("{}[]", sql_type_name(element)),
        ColumnType::Record => "record".into(),
        ColumnType::Date => "date".into(),
        ColumnType::Time => "time".into(),
        ColumnType::TimeTz => "time with time zone".into(),
        ColumnType::Timestamp => "timestamp".into(),
        ColumnType::TimestampTz => "timestamp with time zone".into(),
        ColumnType::Interval => "interval".into(),
        ColumnType::Vector(dim) => format!("vector({dim})"),
        ColumnType::Tensor(dim) => format!("tensor({dim})"),
        ColumnType::Domain { schema, name, .. } => format!("{schema}.{name}"),
    }
}

pub(super) fn fdw_type_name(ty: &uqa_fdw::ColumnType) -> String {
    match ty {
        uqa_fdw::ColumnType::SmallInteger => "smallint".into(),
        uqa_fdw::ColumnType::Integer => "integer".into(),
        uqa_fdw::ColumnType::BigInteger => "bigint".into(),
        uqa_fdw::ColumnType::Oid => "oid".into(),
        uqa_fdw::ColumnType::Xid => "xid".into(),
        uqa_fdw::ColumnType::Real => "real".into(),
        uqa_fdw::ColumnType::DoublePrecision => "double precision".into(),
        uqa_fdw::ColumnType::Numeric { precision, scale } => match (precision, scale) {
            (Some(p), Some(s)) => format!("numeric({p},{s})"),
            (Some(p), None) => format!("numeric({p})"),
            _ => "numeric".into(),
        },
        uqa_fdw::ColumnType::Text => "text".into(),
        uqa_fdw::ColumnType::Name => "name".into(),
        uqa_fdw::ColumnType::Uuid => "uuid".into(),
        uqa_fdw::ColumnType::Varchar(Some(length)) => format!("character varying({length})"),
        uqa_fdw::ColumnType::Varchar(None) => "character varying".into(),
        uqa_fdw::ColumnType::Bpchar => "character".into(),
        uqa_fdw::ColumnType::Character(length) => format!("character({length})"),
        uqa_fdw::ColumnType::Bool => "boolean".into(),
        uqa_fdw::ColumnType::Bytes => "bytea".into(),
        uqa_fdw::ColumnType::InternalChar => r#""char""#.into(),
        uqa_fdw::ColumnType::Regproc => "regproc".into(),
        uqa_fdw::ColumnType::Regclass => "regclass".into(),
        uqa_fdw::ColumnType::Regnamespace => "regnamespace".into(),
        uqa_fdw::ColumnType::Regtype => "regtype".into(),
        uqa_fdw::ColumnType::PgNodeTree => "pg_node_tree".into(),
        uqa_fdw::ColumnType::AclItem => "aclitem".into(),
        uqa_fdw::ColumnType::Int2Vector => "int2vector".into(),
        uqa_fdw::ColumnType::OidVector => "oidvector".into(),
        uqa_fdw::ColumnType::AnyArray => "anyarray".into(),
        uqa_fdw::ColumnType::Json => "json".into(),
        uqa_fdw::ColumnType::JsonB => "jsonb".into(),
        uqa_fdw::ColumnType::Date => "date".into(),
        uqa_fdw::ColumnType::Time => "time".into(),
        uqa_fdw::ColumnType::TimeTz => "time with time zone".into(),
        uqa_fdw::ColumnType::Timestamp => "timestamp".into(),
        uqa_fdw::ColumnType::TimestampTz => "timestamp with time zone".into(),
        uqa_fdw::ColumnType::Interval => "interval".into(),
        uqa_fdw::ColumnType::Vector(dim) => format!("vector({dim})"),
        uqa_fdw::ColumnType::Tensor(dim) => format!("tensor({dim})"),
        uqa_fdw::ColumnType::Domain { schema, name, .. } => format!("{schema}.{name}"),
        uqa_fdw::ColumnType::Array(element) => format!("{}[]", fdw_type_name(element)),
        uqa_fdw::ColumnType::Record => "record".into(),
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
    if col.auto_increment {
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
        flags.push(format!(
            "REFERENCES {}({})",
            reference.table, reference.column
        ));
    }
    flags.join(" ")
}

pub(super) fn expr_display(expr: &Expr) -> String {
    format!("{expr:?}")
}

pub(super) fn options_display(options: &BTreeMap<String, String>) -> String {
    options
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn foreign_table_options_display(options: &BTreeMap<String, String>) -> String {
    let mut out = Vec::new();
    if options
        .get("hive_partitioning")
        .is_some_and(|value| value.eq_ignore_ascii_case("true"))
    {
        out.push("hive".to_string());
    }
    out.join(", ")
}
