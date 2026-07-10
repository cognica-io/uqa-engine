//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `information_schema` and `pg_catalog` virtual row synthesis.

use uqa_core::Value;
use uqa_sql::ast::{ColumnDef as SQLColumnDef, ColumnType, Expr};
use uqa_sql::registry::registered_names;
use uqa_sql::ResultRow;

use crate::Engine;

use super::{column_type_name, value_to_text};

pub(super) fn build_info_schema_rows(engine: &Engine, name: &str) -> Option<Vec<ResultRow>> {
    let lower = name.to_ascii_lowercase();
    let is_information_schema = lower.starts_with("information_schema.");
    let is_pg_catalog = lower.starts_with("pg_catalog.");
    let stripped: &str = lower
        .strip_prefix("information_schema.")
        .or_else(|| lower.strip_prefix("pg_catalog."))
        .unwrap_or(&lower);
    match (is_information_schema, is_pg_catalog, stripped) {
        (true, _, "schemata") => Some(build_info_schemata(engine)),
        (true, _, "tables") => Some(build_info_tables(engine)),
        (true, _, "columns") => Some(build_info_columns(engine)),
        (true, _, "views") => Some(build_info_views(engine)),
        (true, _, "routines") => Some(build_info_routines(engine)),
        (true, _, "sequences") => Some(build_info_sequences(engine)),
        (true, _, "table_constraints") => Some(build_info_table_constraints(engine)),
        (true, _, "key_column_usage") => Some(build_info_key_column_usage(engine)),
        (_, true, "pg_namespace") | (false, false, "pg_namespace") => {
            Some(build_pg_namespace(engine))
        }
        (_, true, "pg_class") | (false, false, "pg_class") => Some(build_pg_class(engine)),
        (_, true, "pg_attribute") | (false, false, "pg_attribute") => {
            Some(build_pg_attribute(engine))
        }
        (_, true, "pg_attrdef") | (false, false, "pg_attrdef") => Some(build_pg_attrdef(engine)),
        (_, true, "pg_constraint") | (false, false, "pg_constraint") => {
            Some(build_pg_constraint(engine))
        }
        (_, true, "pg_index") | (false, false, "pg_index") => Some(build_pg_index(engine)),
        (_, true, "pg_tables") | (false, false, "pg_tables") => Some(build_pg_tables(engine)),
        (_, true, "pg_views") | (false, false, "pg_views") => Some(build_pg_views(engine)),
        (_, true, "pg_indexes") | (false, false, "pg_indexes") => Some(build_pg_indexes(engine)),
        (_, true, "pg_type") | (false, false, "pg_type") => Some(build_pg_type()),
        (_, true, "pg_proc") | (false, false, "pg_proc") => Some(build_pg_proc(engine)),
        (_, true, "pg_database") | (false, false, "pg_database") => Some(build_pg_database()),
        (_, true, "pg_roles") | (false, false, "pg_roles") => Some(build_pg_roles()),
        (_, true, "pg_user") | (false, false, "pg_user") => Some(build_pg_user()),
        (_, true, "pg_settings") | (false, false, "pg_settings") => Some(build_pg_settings(engine)),
        (_, true, "pg_description") | (false, false, "pg_description") => Some(Vec::new()),
        (_, true, "pg_matviews") | (false, false, "pg_matviews") => Some(Vec::new()),
        (_, true, "pg_sequences") | (false, false, "pg_sequences") => {
            Some(build_pg_sequences(engine))
        }
        _ => None,
    }
}

fn catalog_name() -> Value {
    Value::Str("uqa".into())
}

fn str_value(value: impl Into<String>) -> Value {
    Value::Str(value.into())
}

fn int_value(value: i64) -> Value {
    Value::Int(value)
}

fn bool_value(value: bool) -> Value {
    Value::Bool(value)
}

fn list_int(values: &[i64]) -> Value {
    Value::List(values.iter().copied().map(Value::Int).collect())
}

fn row(entries: impl IntoIterator<Item = (&'static str, Value)>) -> ResultRow {
    let mut out = ResultRow::new();
    for (key, value) in entries {
        out.insert(key.to_string(), value);
    }
    out
}

fn split_schema_name(name: &str) -> (String, String) {
    name.split_once('.').map_or_else(
        || ("public".to_string(), name.to_string()),
        |(schema, rel)| (schema.to_string(), rel.to_string()),
    )
}

fn split_index_name(index_name: &str, table_schema: &str) -> (String, String) {
    index_name.split_once('.').map_or_else(
        || (table_schema.to_string(), index_name.to_string()),
        |(schema, rel)| (schema.to_string(), rel.to_string()),
    )
}

fn stable_oid(kind: &str, name: &str) -> i64 {
    let mut hash = 14_695_981_039_346_656_037_u64;
    for byte in kind.bytes().chain([b':']).chain(name.bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(1_099_511_628_211);
    }
    10_000 + i64::try_from(hash % 2_000_000_000).unwrap_or(0)
}

fn schema_oid(schema: &str) -> i64 {
    match schema {
        "pg_catalog" => 11,
        "public" => 2200,
        "information_schema" => 13_377,
        other => stable_oid("namespace", other),
    }
}

fn relation_oid(kind: &str, schema: &str, name: &str) -> i64 {
    stable_oid(kind, &format!("{schema}.{name}"))
}

fn current_user_oid() -> i64 {
    10
}

fn current_user_name() -> &'static str {
    "uqa"
}

fn all_schema_names(engine: &Engine) -> Vec<String> {
    let mut schemas = vec!["pg_catalog".to_string(), "information_schema".to_string()];
    schemas.extend(engine.list_schemas());
    schemas.sort();
    schemas.dedup();
    schemas
}

fn table_columns_for(engine: &Engine, table: &str) -> Vec<SQLColumnDef> {
    engine.describe_table(table).unwrap_or_default()
}

fn pg_type_oid(ty: &ColumnType) -> i64 {
    match ty {
        ColumnType::Integer => 23,
        ColumnType::Text => 25,
        ColumnType::Real => 701,
        ColumnType::Numeric { .. } => 1700,
        ColumnType::Json => 114,
        ColumnType::JsonB => 3802,
        ColumnType::Bytea => 17,
        ColumnType::Date => 1082,
        ColumnType::Time => 1083,
        ColumnType::TimeTz => 1266,
        ColumnType::Timestamp => 1114,
        ColumnType::TimestampTz => 1184,
        ColumnType::Vector(_) => 380_000,
        ColumnType::Tensor(_) => 380_001,
    }
}

fn pg_type_len(ty: &ColumnType) -> i64 {
    match ty {
        ColumnType::Integer => 4,
        ColumnType::Real | ColumnType::Timestamp | ColumnType::TimestampTz => 8,
        ColumnType::Date => 4,
        ColumnType::Time | ColumnType::TimeTz => 8,
        _ => -1,
    }
}

fn info_datetime_precision(ty: &ColumnType) -> Value {
    match ty {
        ColumnType::Time | ColumnType::TimeTz | ColumnType::Timestamp | ColumnType::TimestampTz => {
            Value::Int(6)
        }
        _ => Value::Null,
    }
}

fn info_numeric_precision(ty: &ColumnType) -> Value {
    match ty {
        ColumnType::Integer => Value::Int(32),
        ColumnType::Real => Value::Int(53),
        ColumnType::Numeric {
            precision: Some(precision),
            ..
        } => Value::Int(i64::from(*precision)),
        _ => Value::Null,
    }
}

fn info_numeric_scale(ty: &ColumnType) -> Value {
    match ty {
        ColumnType::Numeric {
            scale: Some(scale), ..
        } => Value::Int(i64::from(*scale)),
        _ => Value::Null,
    }
}

fn info_udt_name(ty: &ColumnType) -> &'static str {
    match ty {
        ColumnType::Integer => "int4",
        ColumnType::Text => "text",
        ColumnType::Real => "float8",
        ColumnType::Numeric { .. } => "numeric",
        ColumnType::Json => "json",
        ColumnType::JsonB => "jsonb",
        ColumnType::Bytea => "bytea",
        ColumnType::Date => "date",
        ColumnType::Time => "time",
        ColumnType::TimeTz => "timetz",
        ColumnType::Timestamp => "timestamp",
        ColumnType::TimestampTz => "timestamptz",
        ColumnType::Vector(_) => "vector",
        ColumnType::Tensor(_) => "tensor",
    }
}

fn default_expr_text(expr: Option<&Expr>) -> Value {
    expr.map_or(Value::Null, |expr| Value::Str(format!("{expr:?}")))
}

fn index_columns(columns_json: &str) -> Vec<String> {
    serde_json::from_str(columns_json).unwrap_or_default()
}

fn indexdef(name: &str, index_type: &str, table: &str, columns: &[String]) -> String {
    let method = if index_type.is_empty() {
        "btree"
    } else {
        index_type
    };
    format!(
        "CREATE INDEX {name} ON {table} USING {method} ({})",
        columns.join(", ")
    )
}

fn build_info_schemata(engine: &Engine) -> Vec<ResultRow> {
    all_schema_names(engine)
        .into_iter()
        .map(|schema| {
            row([
                ("catalog_name", catalog_name()),
                ("schema_name", str_value(schema)),
                ("schema_owner", str_value(current_user_name())),
                ("default_character_set_catalog", catalog_name()),
                ("default_character_set_schema", str_value("pg_catalog")),
                ("default_character_set_name", str_value("UTF8")),
                ("sql_path", Value::Null),
            ])
        })
        .collect()
}

fn build_info_tables(engine: &Engine) -> Vec<ResultRow> {
    let mut out = Vec::new();
    for name in engine.table_names() {
        let (schema, table) = split_schema_name(&name);
        out.push(row([
            ("table_catalog", catalog_name()),
            ("table_schema", str_value(schema)),
            ("table_name", str_value(table)),
            ("table_type", str_value("BASE TABLE")),
            ("self_referencing_column_name", Value::Null),
            ("reference_generation", Value::Null),
            ("user_defined_type_catalog", Value::Null),
            ("user_defined_type_schema", Value::Null),
            ("user_defined_type_name", Value::Null),
            ("is_insertable_into", str_value("YES")),
            ("is_typed", str_value("NO")),
            ("commit_action", Value::Null),
        ]));
    }
    for name in engine.list_views() {
        let (schema, view) = split_schema_name(&name);
        out.push(row([
            ("table_catalog", catalog_name()),
            ("table_schema", str_value(schema)),
            ("table_name", str_value(view)),
            ("table_type", str_value("VIEW")),
            ("self_referencing_column_name", Value::Null),
            ("reference_generation", Value::Null),
            ("user_defined_type_catalog", Value::Null),
            ("user_defined_type_schema", Value::Null),
            ("user_defined_type_name", Value::Null),
            ("is_insertable_into", str_value("NO")),
            ("is_typed", str_value("NO")),
            ("commit_action", Value::Null),
        ]));
    }
    for name in engine.list_foreign_tables() {
        let (schema, table) = split_schema_name(&name);
        out.push(row([
            ("table_catalog", catalog_name()),
            ("table_schema", str_value(schema)),
            ("table_name", str_value(table)),
            ("table_type", str_value("FOREIGN")),
            ("self_referencing_column_name", Value::Null),
            ("reference_generation", Value::Null),
            ("user_defined_type_catalog", Value::Null),
            ("user_defined_type_schema", Value::Null),
            ("user_defined_type_name", Value::Null),
            ("is_insertable_into", str_value("YES")),
            ("is_typed", str_value("NO")),
            ("commit_action", Value::Null),
        ]));
    }
    out.sort_by(|a, b| {
        value_to_text(a.get("table_schema").unwrap_or(&Value::Null))
            .cmp(&value_to_text(
                b.get("table_schema").unwrap_or(&Value::Null),
            ))
            .then_with(|| {
                value_to_text(a.get("table_name").unwrap_or(&Value::Null))
                    .cmp(&value_to_text(b.get("table_name").unwrap_or(&Value::Null)))
            })
    });
    out
}

fn build_pg_tables(engine: &Engine) -> Vec<ResultRow> {
    let mut out: Vec<ResultRow> = Vec::new();
    let mut names = engine.table_names();
    names.sort();
    for name in names {
        let (schema, table) = split_schema_name(&name);
        out.push(row([
            ("schemaname", str_value(schema.clone())),
            ("tablename", str_value(table)),
            ("tableowner", str_value(current_user_name())),
            ("tablespace", Value::Null),
            (
                "hasindexes",
                bool_value(
                    engine
                        .list_catalog_indexes()
                        .iter()
                        .any(|idx| idx.table_name == name),
                ),
            ),
            ("hasrules", bool_value(false)),
            ("hastriggers", bool_value(false)),
            ("rowsecurity", bool_value(false)),
            ("table_catalog", catalog_name()),
            ("table_schema", str_value(schema)),
            ("table_type", str_value("BASE TABLE")),
        ]));
    }
    out
}

fn build_info_columns(engine: &Engine) -> Vec<ResultRow> {
    let mut out: Vec<ResultRow> = Vec::new();
    let mut tables = engine.table_names();
    tables.sort();
    for tname in tables {
        let Some(cols) = engine.describe_table(&tname) else {
            continue;
        };
        for (idx, col) in cols.iter().enumerate() {
            let (schema, table) = split_schema_name(&tname);
            out.push(row([
                ("table_catalog", catalog_name()),
                ("table_schema", str_value(schema)),
                ("table_name", str_value(table)),
                ("column_name", str_value(col.name.clone())),
                ("ordinal_position", int_value((idx + 1) as i64)),
                ("column_default", default_expr_text(col.default.as_ref())),
                (
                    "is_nullable",
                    str_value(if col.not_null || col.primary_key {
                        "NO"
                    } else {
                        "YES"
                    }),
                ),
                ("data_type", str_value(column_type_name(&col.ty))),
                ("character_maximum_length", Value::Null),
                ("character_octet_length", Value::Null),
                ("numeric_precision", info_numeric_precision(&col.ty)),
                ("numeric_precision_radix", Value::Int(10)),
                ("numeric_scale", info_numeric_scale(&col.ty)),
                ("datetime_precision", info_datetime_precision(&col.ty)),
                ("interval_type", Value::Null),
                ("interval_precision", Value::Null),
                ("character_set_catalog", Value::Null),
                ("character_set_schema", Value::Null),
                ("character_set_name", Value::Null),
                ("collation_catalog", Value::Null),
                ("collation_schema", Value::Null),
                ("collation_name", Value::Null),
                ("domain_catalog", Value::Null),
                ("domain_schema", Value::Null),
                ("domain_name", Value::Null),
                ("udt_catalog", catalog_name()),
                ("udt_schema", str_value("pg_catalog")),
                ("udt_name", str_value(info_udt_name(&col.ty))),
                ("scope_catalog", Value::Null),
                ("scope_schema", Value::Null),
                ("scope_name", Value::Null),
                ("maximum_cardinality", Value::Null),
                ("dtd_identifier", str_value((idx + 1).to_string())),
                (
                    "is_self_referencing",
                    str_value(if col.references.is_some() {
                        "YES"
                    } else {
                        "NO"
                    }),
                ),
                (
                    "is_identity",
                    str_value(if col.auto_increment { "YES" } else { "NO" }),
                ),
                ("identity_generation", Value::Null),
                ("identity_start", Value::Null),
                ("identity_increment", Value::Null),
                ("identity_maximum", Value::Null),
                ("identity_minimum", Value::Null),
                ("identity_cycle", str_value("NO")),
                ("is_generated", str_value("NEVER")),
                ("generation_expression", Value::Null),
                ("is_updatable", str_value("YES")),
            ]));
        }
    }
    out
}

fn build_info_views(engine: &Engine) -> Vec<ResultRow> {
    engine
        .list_views()
        .into_iter()
        .map(|name| {
            let (schema, view) = split_schema_name(&name);
            row([
                ("table_catalog", catalog_name()),
                ("table_schema", str_value(schema)),
                ("table_name", str_value(view)),
                (
                    "view_definition",
                    str_value(
                        engine
                            .view(&name)
                            .map_or_else(String::new, |stmt| format!("{stmt:?}")),
                    ),
                ),
                ("check_option", str_value("NONE")),
                ("is_updatable", str_value("NO")),
                ("is_insertable_into", str_value("NO")),
                ("is_trigger_updatable", str_value("NO")),
                ("is_trigger_deletable", str_value("NO")),
                ("is_trigger_insertable_into", str_value("NO")),
            ])
        })
        .collect()
}

fn build_info_routines(engine: &Engine) -> Vec<ResultRow> {
    let mut rows: Vec<ResultRow> = registered_names()
        .into_iter()
        .map(|name| {
            row([
                ("specific_catalog", catalog_name()),
                ("specific_schema", str_value("pg_catalog")),
                ("specific_name", str_value(format!("{name}_0"))),
                ("routine_catalog", catalog_name()),
                ("routine_schema", str_value("pg_catalog")),
                ("routine_name", str_value(name)),
                ("routine_type", str_value("FUNCTION")),
                ("module_catalog", Value::Null),
                ("module_schema", Value::Null),
                ("module_name", Value::Null),
                ("udt_catalog", catalog_name()),
                ("udt_schema", str_value("pg_catalog")),
                ("udt_name", str_value("text")),
                ("data_type", str_value("text")),
                ("routine_body", str_value("EXTERNAL")),
                ("routine_definition", Value::Null),
                ("external_name", Value::Null),
                ("external_language", str_value("rust")),
                ("is_deterministic", str_value("NO")),
                ("sql_data_access", str_value("READS SQL DATA")),
                ("is_null_call", str_value("YES")),
                ("schema_level_routine", str_value("YES")),
                ("max_dynamic_result_sets", Value::Int(0)),
                ("is_udt_dependent", str_value("NO")),
                ("result_cast_from_null", str_value("NO")),
            ])
        })
        .collect();
    for function in engine.list_sql_functions() {
        let def = &function.def;
        let routine_type = if def.is_procedure {
            "PROCEDURE"
        } else {
            "FUNCTION"
        };
        let (routine_body, external_language) = if def.language == "sql" {
            ("SQL", "SQL".to_string())
        } else {
            ("EXTERNAL", def.language.to_ascii_uppercase())
        };
        let definition = match &def.body {
            uqa_sql::ast::FunctionBody::Source(source) => str_value(source.clone()),
            uqa_sql::ast::FunctionBody::Statements(_) => Value::Null,
        };
        let data_type = match &def.returns {
            uqa_sql::ast::FunctionReturns::Scalar { type_name }
            | uqa_sql::ast::FunctionReturns::SetOf { type_name } => str_value(type_name.clone()),
            uqa_sql::ast::FunctionReturns::Table => str_value("record"),
            uqa_sql::ast::FunctionReturns::None => Value::Null,
        };
        rows.push(row([
            ("specific_catalog", catalog_name()),
            ("specific_schema", str_value("public")),
            (
                "specific_name",
                str_value(format!("{}_{}", def.name, def.signature_arity())),
            ),
            ("routine_catalog", catalog_name()),
            ("routine_schema", str_value("public")),
            ("routine_name", str_value(def.name.clone())),
            ("routine_type", str_value(routine_type)),
            ("module_catalog", Value::Null),
            ("module_schema", Value::Null),
            ("module_name", Value::Null),
            ("udt_catalog", catalog_name()),
            ("udt_schema", str_value("pg_catalog")),
            ("udt_name", data_type.clone()),
            ("data_type", data_type),
            ("routine_body", str_value(routine_body)),
            ("routine_definition", definition),
            ("external_name", Value::Null),
            ("external_language", str_value(external_language)),
            (
                "is_deterministic",
                str_value(
                    if matches!(def.volatility, uqa_sql::ast::FunctionVolatility::Immutable) {
                        "YES"
                    } else {
                        "NO"
                    },
                ),
            ),
            ("sql_data_access", str_value("MODIFIES SQL DATA")),
            (
                "is_null_call",
                str_value(if def.strict { "YES" } else { "NO" }),
            ),
            ("schema_level_routine", str_value("YES")),
            ("max_dynamic_result_sets", Value::Int(0)),
            ("is_udt_dependent", str_value("NO")),
            ("result_cast_from_null", Value::Null),
        ]));
    }
    rows
}

fn build_info_sequences(engine: &Engine) -> Vec<ResultRow> {
    engine
        .list_sequences()
        .into_iter()
        .map(|name| {
            let (schema, sequence) = split_schema_name(&name);
            row([
                ("sequence_catalog", catalog_name()),
                ("sequence_schema", str_value(schema)),
                ("sequence_name", str_value(sequence)),
                ("data_type", str_value("bigint")),
                ("numeric_precision", Value::Int(64)),
                ("numeric_precision_radix", Value::Int(2)),
                ("numeric_scale", Value::Int(0)),
                ("start_value", Value::Null),
                ("minimum_value", Value::Null),
                ("maximum_value", Value::Null),
                ("increment", Value::Null),
                ("cycle_option", str_value("NO")),
            ])
        })
        .collect()
}

fn column_constraint_rows(engine: &Engine) -> Vec<(String, String, String, String, String, i64)> {
    let mut out = Vec::new();
    for table_name in engine.table_names() {
        let (schema, table) = split_schema_name(&table_name);
        for (idx, col) in table_columns_for(engine, &table_name).iter().enumerate() {
            let ordinal = (idx + 1) as i64;
            if col.primary_key {
                out.push((
                    schema.clone(),
                    table.clone(),
                    col.name.clone(),
                    format!("{table}_{}_pkey", col.name),
                    "PRIMARY KEY".to_string(),
                    ordinal,
                ));
            }
            if col.unique {
                out.push((
                    schema.clone(),
                    table.clone(),
                    col.name.clone(),
                    format!("{table}_{}_key", col.name),
                    "UNIQUE".to_string(),
                    ordinal,
                ));
            }
            if col.references.is_some() {
                out.push((
                    schema.clone(),
                    table.clone(),
                    col.name.clone(),
                    format!("{table}_{}_fkey", col.name),
                    "FOREIGN KEY".to_string(),
                    ordinal,
                ));
            }
            if col.check.is_some() {
                out.push((
                    schema.clone(),
                    table.clone(),
                    col.name.clone(),
                    format!("{table}_{}_check", col.name),
                    "CHECK".to_string(),
                    ordinal,
                ));
            }
        }
    }
    out
}

fn build_info_table_constraints(engine: &Engine) -> Vec<ResultRow> {
    column_constraint_rows(engine)
        .into_iter()
        .map(|(schema, table, _column, constraint, kind, _ordinal)| {
            row([
                ("constraint_catalog", catalog_name()),
                ("constraint_schema", str_value(schema.clone())),
                ("constraint_name", str_value(constraint)),
                ("table_schema", str_value(schema)),
                ("table_name", str_value(table)),
                ("constraint_type", str_value(kind)),
                ("is_deferrable", str_value("NO")),
                ("initially_deferred", str_value("NO")),
                ("enforced", str_value("YES")),
                ("nulls_distinct", str_value("YES")),
            ])
        })
        .collect()
}

fn build_info_key_column_usage(engine: &Engine) -> Vec<ResultRow> {
    column_constraint_rows(engine)
        .into_iter()
        .filter(|(_, _, _, _, kind, _)| kind != "CHECK")
        .map(|(schema, table, column, constraint, _kind, ordinal)| {
            row([
                ("constraint_catalog", catalog_name()),
                ("constraint_schema", str_value(schema.clone())),
                ("constraint_name", str_value(constraint)),
                ("table_catalog", catalog_name()),
                ("table_schema", str_value(schema)),
                ("table_name", str_value(table)),
                ("column_name", str_value(column)),
                ("ordinal_position", int_value(ordinal)),
                ("position_in_unique_constraint", Value::Null),
            ])
        })
        .collect()
}

fn build_pg_namespace(engine: &Engine) -> Vec<ResultRow> {
    all_schema_names(engine)
        .into_iter()
        .map(|schema| {
            row([
                ("oid", int_value(schema_oid(&schema))),
                ("nspname", str_value(schema)),
                ("nspowner", int_value(current_user_oid())),
                ("nspacl", Value::Null),
            ])
        })
        .collect()
}

fn build_pg_class(engine: &Engine) -> Vec<ResultRow> {
    let mut out = Vec::new();
    for name in engine.table_names() {
        let (schema, table) = split_schema_name(&name);
        let columns = table_columns_for(engine, &name);
        out.push(pg_class_row(
            &schema,
            &table,
            "r",
            columns.len() as i64,
            engine.document_count(&name) as f64,
            engine
                .list_catalog_indexes()
                .iter()
                .any(|idx| idx.table_name == name),
        ));
    }
    for name in engine.list_views() {
        let (schema, view) = split_schema_name(&name);
        out.push(pg_class_row(&schema, &view, "v", 0, 0.0, false));
    }
    for name in engine.list_foreign_tables() {
        let (schema, table) = split_schema_name(&name);
        out.push(pg_class_row(
            &schema,
            &table,
            "f",
            engine.foreign_table_columns(&name).len() as i64,
            0.0,
            false,
        ));
    }
    for sequence in engine.list_sequences() {
        let (schema, name) = split_schema_name(&sequence);
        out.push(pg_class_row(&schema, &name, "S", 0, 0.0, false));
    }
    for idx in engine.list_catalog_indexes() {
        let (table_schema, _) = split_schema_name(&idx.table_name);
        let (schema, index_name) = split_index_name(&idx.name, &table_schema);
        out.push(pg_class_row(&schema, &index_name, "i", 0, 0.0, false));
    }
    out
}

fn pg_class_row(
    schema: &str,
    name: &str,
    relkind: &str,
    natts: i64,
    tuples: f64,
    has_index: bool,
) -> ResultRow {
    row([
        ("oid", int_value(relation_oid(relkind, schema, name))),
        ("relname", str_value(name)),
        ("relnamespace", int_value(schema_oid(schema))),
        (
            "reltype",
            int_value(stable_oid("rowtype", &format!("{schema}.{name}"))),
        ),
        ("reloftype", int_value(0)),
        ("relowner", int_value(current_user_oid())),
        ("relam", int_value(0)),
        ("relfilenode", int_value(0)),
        ("reltablespace", int_value(0)),
        ("relpages", int_value(0)),
        ("reltuples", Value::Float(tuples)),
        ("relallvisible", int_value(0)),
        ("reltoastrelid", int_value(0)),
        ("relhasindex", bool_value(has_index)),
        ("relisshared", bool_value(false)),
        ("relpersistence", str_value("p")),
        ("relkind", str_value(relkind)),
        ("relnatts", int_value(natts)),
        ("relchecks", int_value(0)),
        ("relhasrules", bool_value(relkind == "v")),
        ("relhastriggers", bool_value(false)),
        ("relhassubclass", bool_value(false)),
        ("relrowsecurity", bool_value(false)),
        ("relforcerowsecurity", bool_value(false)),
        ("relispopulated", bool_value(true)),
        ("relreplident", str_value("d")),
        ("relispartition", bool_value(false)),
        ("relrewrite", int_value(0)),
        ("relfrozenxid", int_value(0)),
        ("relminmxid", int_value(0)),
        ("relacl", Value::Null),
        ("reloptions", Value::Null),
        ("relpartbound", Value::Null),
    ])
}

fn build_pg_attribute(engine: &Engine) -> Vec<ResultRow> {
    let mut out = Vec::new();
    for table_name in engine.table_names() {
        let (schema, table) = split_schema_name(&table_name);
        let relid = relation_oid("r", &schema, &table);
        for (idx, col) in table_columns_for(engine, &table_name).iter().enumerate() {
            out.push(pg_attribute_row(relid, (idx + 1) as i64, col));
        }
    }
    for table_name in engine.list_foreign_tables() {
        let (schema, table) = split_schema_name(&table_name);
        let relid = relation_oid("f", &schema, &table);
        for (idx, col) in engine.foreign_table_columns(&table_name).iter().enumerate() {
            let col = SQLColumnDef {
                name: col.clone(),
                ty: ColumnType::Text,
                primary_key: false,
                not_null: false,
                auto_increment: false,
                unique: false,
                default: None,
                check: None,
                references: None,
            };
            out.push(pg_attribute_row(relid, (idx + 1) as i64, &col));
        }
    }
    out
}

fn pg_attribute_row(relid: i64, attnum: i64, col: &SQLColumnDef) -> ResultRow {
    row([
        ("attrelid", int_value(relid)),
        ("attname", str_value(col.name.clone())),
        ("atttypid", int_value(pg_type_oid(&col.ty))),
        ("attstattarget", int_value(-1)),
        ("attlen", int_value(pg_type_len(&col.ty))),
        ("attnum", int_value(attnum)),
        ("attndims", int_value(0)),
        ("attcacheoff", int_value(-1)),
        ("atttypmod", int_value(-1)),
        (
            "attbyval",
            bool_value(matches!(col.ty, ColumnType::Integer | ColumnType::Real)),
        ),
        ("attalign", str_value("i")),
        ("attstorage", str_value("x")),
        ("attcompression", str_value("")),
        ("attnotnull", bool_value(col.not_null || col.primary_key)),
        (
            "atthasdef",
            bool_value(col.default.is_some() || col.auto_increment),
        ),
        ("atthasmissing", bool_value(false)),
        (
            "attidentity",
            str_value(if col.auto_increment { "d" } else { "" }),
        ),
        ("attgenerated", str_value("")),
        ("attisdropped", bool_value(false)),
        ("attislocal", bool_value(true)),
        ("attinhcount", int_value(0)),
        ("attcollation", int_value(0)),
        ("attacl", Value::Null),
        ("attoptions", Value::Null),
        ("attfdwoptions", Value::Null),
    ])
}

fn build_pg_attrdef(engine: &Engine) -> Vec<ResultRow> {
    let mut out = Vec::new();
    for table_name in engine.table_names() {
        let (schema, table) = split_schema_name(&table_name);
        let relid = relation_oid("r", &schema, &table);
        for (idx, col) in table_columns_for(engine, &table_name).iter().enumerate() {
            if col.default.is_none() && !col.auto_increment {
                continue;
            }
            let default = if col.auto_increment {
                format!("nextval('{}_{}_seq')", table, col.name)
            } else {
                value_to_text(&default_expr_text(col.default.as_ref()))
            };
            out.push(row([
                (
                    "oid",
                    int_value(stable_oid("attrdef", &format!("{table_name}.{}", col.name))),
                ),
                ("adrelid", int_value(relid)),
                ("adnum", int_value((idx + 1) as i64)),
                ("adbin", str_value(default.clone())),
                ("adsrc", str_value(default)),
            ]));
        }
    }
    out
}

fn build_pg_constraint(engine: &Engine) -> Vec<ResultRow> {
    column_constraint_rows(engine)
        .into_iter()
        .map(|(schema, table, _column, constraint, kind, ordinal)| {
            let contype = match kind.as_str() {
                "PRIMARY KEY" => "p",
                "UNIQUE" => "u",
                "FOREIGN KEY" => "f",
                "CHECK" => "c",
                _ => "c",
            };
            row([
                (
                    "oid",
                    int_value(stable_oid("constraint", &format!("{schema}.{constraint}"))),
                ),
                ("conname", str_value(constraint)),
                ("connamespace", int_value(schema_oid(&schema))),
                ("contype", str_value(contype)),
                ("condeferrable", bool_value(false)),
                ("condeferred", bool_value(false)),
                ("convalidated", bool_value(true)),
                ("conrelid", int_value(relation_oid("r", &schema, &table))),
                ("contypid", int_value(0)),
                ("conindid", int_value(0)),
                ("conparentid", int_value(0)),
                ("confrelid", int_value(0)),
                ("confupdtype", str_value("a")),
                ("confdeltype", str_value("a")),
                ("confmatchtype", str_value("s")),
                ("conislocal", bool_value(true)),
                ("coninhcount", int_value(0)),
                ("connoinherit", bool_value(true)),
                ("conkey", list_int(&[ordinal])),
                ("confkey", Value::Null),
                ("conpfeqop", Value::Null),
                ("conppeqop", Value::Null),
                ("conffeqop", Value::Null),
                ("conexclop", Value::Null),
                ("conbin", Value::Null),
            ])
        })
        .collect()
}

fn build_pg_index(engine: &Engine) -> Vec<ResultRow> {
    engine
        .list_catalog_indexes()
        .into_iter()
        .map(|idx| {
            let columns = index_columns(&idx.columns_json);
            let (schema, table) = split_schema_name(&idx.table_name);
            let (index_schema, index_name) = split_index_name(&idx.name, &schema);
            let table_cols = engine.table_columns(&idx.table_name);
            let keys: Vec<i64> = columns
                .iter()
                .filter_map(|col| table_cols.iter().position(|name| name == col))
                .map(|idx| (idx + 1) as i64)
                .collect();
            row([
                (
                    "indexrelid",
                    int_value(relation_oid("i", &index_schema, &index_name)),
                ),
                ("indrelid", int_value(relation_oid("r", &schema, &table))),
                ("indnatts", int_value(columns.len() as i64)),
                ("indnkeyatts", int_value(columns.len() as i64)),
                ("indisunique", bool_value(false)),
                ("indnullsnotdistinct", bool_value(false)),
                ("indisprimary", bool_value(false)),
                ("indisexclusion", bool_value(false)),
                ("indimmediate", bool_value(true)),
                ("indisclustered", bool_value(false)),
                ("indisvalid", bool_value(true)),
                ("indcheckxmin", bool_value(false)),
                ("indisready", bool_value(true)),
                ("indislive", bool_value(true)),
                ("indisreplident", bool_value(false)),
                (
                    "indkey",
                    Value::List(keys.into_iter().map(Value::Int).collect()),
                ),
                ("indcollation", Value::Null),
                ("indclass", Value::Null),
                ("indoption", Value::Null),
                ("indexprs", Value::Null),
                ("indpred", Value::Null),
            ])
        })
        .collect()
}

fn build_pg_views(engine: &Engine) -> Vec<ResultRow> {
    engine
        .list_views()
        .into_iter()
        .map(|name| {
            let (schema, view) = split_schema_name(&name);
            row([
                ("schemaname", str_value(schema)),
                ("viewname", str_value(view)),
                ("viewowner", str_value(current_user_name())),
                (
                    "definition",
                    str_value(
                        engine
                            .view(&name)
                            .map_or_else(String::new, |stmt| format!("{stmt:?}")),
                    ),
                ),
            ])
        })
        .collect()
}

fn build_pg_indexes(engine: &Engine) -> Vec<ResultRow> {
    engine
        .list_catalog_indexes()
        .into_iter()
        .map(|idx| {
            let columns = index_columns(&idx.columns_json);
            let (schema, table) = split_schema_name(&idx.table_name);
            let (_, index_name) = split_index_name(&idx.name, &schema);
            row([
                ("schemaname", str_value(schema)),
                ("tablename", str_value(table.clone())),
                ("indexname", str_value(index_name.clone())),
                ("tablespace", Value::Null),
                (
                    "indexdef",
                    str_value(indexdef(&index_name, &idx.index_type, &table, &columns)),
                ),
            ])
        })
        .collect()
}

fn build_pg_type() -> Vec<ResultRow> {
    let types = [
        (16_i64, "bool", 1_i64, "B"),
        (17, "bytea", -1, "U"),
        (20, "int8", 8, "N"),
        (21, "int2", 2, "N"),
        (23, "int4", 4, "N"),
        (25, "text", -1, "S"),
        (700, "float4", 4, "N"),
        (701, "float8", 8, "N"),
        (1043, "varchar", -1, "S"),
        (1082, "date", 4, "D"),
        (1083, "time", 8, "D"),
        (1114, "timestamp", 8, "D"),
        (1184, "timestamptz", 8, "D"),
        (1266, "timetz", 8, "D"),
        (114, "json", -1, "U"),
        (3802, "jsonb", -1, "U"),
        (1700, "numeric", -1, "N"),
        (2278, "void", 4, "P"),
        (380_000, "vector", -1, "U"),
    ];
    types
        .into_iter()
        .map(|(oid, name, len, category)| {
            row([
                ("oid", int_value(oid)),
                ("typname", str_value(name)),
                ("typnamespace", int_value(schema_oid("pg_catalog"))),
                ("typowner", int_value(current_user_oid())),
                ("typlen", int_value(len)),
                ("typbyval", bool_value(len > 0 && len <= 8)),
                ("typtype", str_value("b")),
                ("typcategory", str_value(category)),
                ("typispreferred", bool_value(false)),
                ("typisdefined", bool_value(true)),
                ("typdelim", str_value(",")),
                ("typrelid", int_value(0)),
                ("typsubscript", str_value("-")),
                ("typelem", int_value(0)),
                ("typarray", int_value(0)),
                ("typinput", int_value(0)),
                ("typoutput", int_value(0)),
                ("typreceive", int_value(0)),
                ("typsend", int_value(0)),
                ("typmodin", int_value(0)),
                ("typmodout", int_value(0)),
                ("typanalyze", int_value(0)),
                ("typalign", str_value("i")),
                ("typstorage", str_value("x")),
                ("typnotnull", bool_value(false)),
                ("typbasetype", int_value(0)),
                ("typtypmod", int_value(-1)),
                ("typndims", int_value(0)),
                ("typcollation", int_value(0)),
                ("typdefaultbin", Value::Null),
                ("typdefault", Value::Null),
                ("typacl", Value::Null),
            ])
        })
        .collect()
}

fn build_pg_proc(engine: &Engine) -> Vec<ResultRow> {
    let mut rows: Vec<ResultRow> = registered_names()
        .into_iter()
        .map(|name| {
            row([
                ("oid", int_value(stable_oid("proc", name))),
                ("proname", str_value(name)),
                ("pronamespace", int_value(schema_oid("pg_catalog"))),
                ("proowner", int_value(current_user_oid())),
                ("prolang", int_value(0)),
                ("procost", Value::Float(1.0)),
                ("prorows", Value::Float(0.0)),
                ("provariadic", int_value(0)),
                ("prosupport", str_value("-")),
                ("prokind", str_value("f")),
                ("prosecdef", bool_value(false)),
                ("proleakproof", bool_value(false)),
                ("proisstrict", bool_value(false)),
                ("proretset", bool_value(false)),
                ("provolatile", str_value("s")),
                ("proparallel", str_value("s")),
                ("pronargs", int_value(0)),
                ("pronargdefaults", int_value(0)),
                ("prorettype", int_value(25)),
                ("proargtypes", Value::List(Vec::new())),
                ("proallargtypes", Value::Null),
                ("proargmodes", Value::Null),
                ("proargnames", Value::Null),
                ("proargdefaults", Value::Null),
                ("protrftypes", Value::Null),
                ("prosrc", str_value(name)),
                ("probin", Value::Null),
                ("prosqlbody", Value::Null),
                ("proconfig", Value::Null),
                ("proacl", Value::Null),
            ])
        })
        .collect();
    for function in engine.list_sql_functions() {
        let def = &function.def;
        let source = match &def.body {
            uqa_sql::ast::FunctionBody::Source(source) => source.clone(),
            uqa_sql::ast::FunctionBody::Statements(_) => String::new(),
        };
        let volatile = match def.volatility {
            uqa_sql::ast::FunctionVolatility::Immutable => "i",
            uqa_sql::ast::FunctionVolatility::Stable => "s",
            uqa_sql::ast::FunctionVolatility::Volatile => "v",
        };
        let arg_names: Vec<Value> = def
            .params
            .iter()
            .map(|p| str_value(p.name.clone()))
            .collect();
        let arg_modes: Vec<Value> = def
            .params
            .iter()
            .map(|p| {
                str_value(match p.mode {
                    uqa_sql::ast::FunctionParamMode::In => "i",
                    uqa_sql::ast::FunctionParamMode::Out => "o",
                    uqa_sql::ast::FunctionParamMode::InOut => "b",
                    uqa_sql::ast::FunctionParamMode::Table => "t",
                })
            })
            .collect();
        let defaults = def.params.iter().filter(|p| p.default.is_some()).count();
        rows.push(row([
            (
                "oid",
                int_value(stable_oid(
                    "proc",
                    &format!("{}_{}", def.name, def.signature_arity()),
                )),
            ),
            ("proname", str_value(def.name.clone())),
            ("pronamespace", int_value(schema_oid("public"))),
            ("proowner", int_value(current_user_oid())),
            ("prolang", int_value(0)),
            ("procost", Value::Float(100.0)),
            (
                "prorows",
                Value::Float(if def.returns_set() { 1000.0 } else { 0.0 }),
            ),
            ("provariadic", int_value(0)),
            ("prosupport", str_value("-")),
            (
                "prokind",
                str_value(if def.is_procedure { "p" } else { "f" }),
            ),
            ("prosecdef", bool_value(false)),
            ("proleakproof", bool_value(false)),
            ("proisstrict", bool_value(def.strict)),
            ("proretset", bool_value(def.returns_set())),
            ("provolatile", str_value(volatile)),
            ("proparallel", str_value("u")),
            ("pronargs", int_value(def.signature_arity() as i64)),
            ("pronargdefaults", int_value(defaults as i64)),
            ("prorettype", int_value(25)),
            ("proargtypes", Value::List(Vec::new())),
            ("proallargtypes", Value::Null),
            ("proargmodes", Value::List(arg_modes)),
            ("proargnames", Value::List(arg_names)),
            ("proargdefaults", Value::Null),
            ("protrftypes", Value::Null),
            ("prosrc", str_value(source)),
            ("probin", Value::Null),
            ("prosqlbody", Value::Null),
            ("proconfig", Value::Null),
            ("proacl", Value::Null),
        ]));
    }
    rows
}

fn build_pg_database() -> Vec<ResultRow> {
    vec![row([
        ("oid", int_value(5)),
        ("datname", str_value("uqa")),
        ("datdba", int_value(current_user_oid())),
        ("encoding", int_value(6)),
        ("datlocprovider", str_value("c")),
        ("datistemplate", bool_value(false)),
        ("datallowconn", bool_value(true)),
        ("datconnlimit", int_value(-1)),
        ("datfrozenxid", int_value(0)),
        ("datminmxid", int_value(0)),
        ("dattablespace", int_value(0)),
        ("datcollate", str_value("C")),
        ("datctype", str_value("C")),
        ("daticulocale", Value::Null),
        ("datcollversion", Value::Null),
        ("datacl", Value::Null),
    ])]
}

fn build_pg_roles() -> Vec<ResultRow> {
    vec![row([
        ("oid", int_value(current_user_oid())),
        ("rolname", str_value(current_user_name())),
        ("rolsuper", bool_value(true)),
        ("rolinherit", bool_value(true)),
        ("rolcreaterole", bool_value(true)),
        ("rolcreatedb", bool_value(true)),
        ("rolcanlogin", bool_value(true)),
        ("rolreplication", bool_value(false)),
        ("rolconnlimit", int_value(-1)),
        ("rolpassword", str_value("********")),
        ("rolvaliduntil", Value::Null),
        ("rolbypassrls", bool_value(true)),
        ("rolconfig", Value::Null),
    ])]
}

fn build_pg_user() -> Vec<ResultRow> {
    vec![row([
        ("usename", str_value(current_user_name())),
        ("usesysid", int_value(current_user_oid())),
        ("usecreatedb", bool_value(true)),
        ("usesuper", bool_value(true)),
        ("userepl", bool_value(false)),
        ("usebypassrls", bool_value(true)),
        ("passwd", str_value("********")),
        ("valuntil", Value::Null),
        ("useconfig", Value::Null),
    ])]
}

fn build_pg_settings(engine: &Engine) -> Vec<ResultRow> {
    let settings = [
        ("server_version", "17.0-uqa", "Version and compatibility"),
        ("server_encoding", "UTF8", "Client connection defaults"),
        ("client_encoding", "UTF8", "Client connection defaults"),
        ("DateStyle", "ISO, MDY", "Locale and formatting"),
        ("TimeZone", "UTC", "Locale and formatting"),
        (
            "search_path",
            &engine.show_variable("search_path"),
            "Client connection defaults",
        ),
    ];
    settings
        .into_iter()
        .map(|(name, setting, category)| {
            row([
                ("name", str_value(name)),
                ("setting", str_value(setting)),
                ("unit", Value::Null),
                ("category", str_value(category)),
                ("short_desc", str_value(name)),
                ("extra_desc", Value::Null),
                ("context", str_value("user")),
                ("vartype", str_value("string")),
                ("source", str_value("default")),
                ("min_val", Value::Null),
                ("max_val", Value::Null),
                ("enumvals", Value::Null),
                ("boot_val", str_value(setting)),
                ("reset_val", str_value(setting)),
                ("sourcefile", Value::Null),
                ("sourceline", Value::Null),
                ("pending_restart", bool_value(false)),
            ])
        })
        .collect()
}

fn build_pg_sequences(engine: &Engine) -> Vec<ResultRow> {
    engine
        .list_sequences()
        .into_iter()
        .map(|name| {
            let (schema, sequence) = split_schema_name(&name);
            row([
                ("schemaname", str_value(schema)),
                ("sequencename", str_value(sequence)),
                ("sequenceowner", str_value(current_user_name())),
                ("data_type", str_value("bigint")),
                ("start_value", Value::Null),
                ("min_value", Value::Null),
                ("max_value", Value::Null),
                ("increment_by", Value::Null),
                ("cycle", bool_value(false)),
                ("cache_size", Value::Int(1)),
                ("last_value", Value::Null),
            ])
        })
        .collect()
}
