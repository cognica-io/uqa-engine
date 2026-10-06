//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `information_schema.routines`: built-in catalog routines, registered operator functions and stored SQL routines.

use super::super::builtin_routines::PG18_BUILTIN_ROUTINE_GROUPS;
use super::super::helpers::oids::split_schema_name;
use super::super::helpers::rows::{catalog_name, row, str_value};
use super::super::helpers::type_metadata::{catalog_regtype_name, catalog_type_name};
use super::super::pg_proc::user_routine_catalog_oid;
use crate::catalog::{CatalogReadView, RelationNameResolution};
use uqa_core::Value;
use uqa_sql::catalog::languages::SQL_LANGUAGE;
use uqa_sql::registry::registered_names;
use uqa_sql::{ResultRow, SQLError};

#[expect(
    clippy::too_many_lines,
    reason = "preserves catalog column and OID order"
)]
pub fn build_info_routines(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
) -> Result<Vec<ResultRow>, SQLError> {
    let builtin_owner = catalog.role_is_enabled_for(
        resolution.current_user(),
        &uqa_sql::catalog::roles::RoleIdentity::BOOTSTRAP,
    );
    let mut rows: Vec<ResultRow> = PG18_BUILTIN_ROUTINE_GROUPS
        .iter()
        .flat_map(|group| group.iter())
        .map(|routine| {
            let regtype_name = catalog_regtype_name(routine.return_type);
            row([
                ("specific_catalog", catalog_name()),
                ("specific_schema", str_value("pg_catalog")),
                (
                    "specific_name",
                    str_value(format!("{}_{}", routine.name, routine.oid)),
                ),
                ("routine_catalog", catalog_name()),
                ("routine_schema", str_value("pg_catalog")),
                ("routine_name", str_value(routine.name)),
                (
                    "routine_type",
                    if routine.kind == "f" {
                        str_value("FUNCTION")
                    } else {
                        Value::Null
                    },
                ),
                ("module_catalog", Value::Null),
                ("module_schema", Value::Null),
                ("module_name", Value::Null),
                ("udt_catalog", Value::Null),
                ("udt_schema", Value::Null),
                ("udt_name", Value::Null),
                (
                    "data_type",
                    str_value(catalog_type_name(routine.return_type)),
                ),
                (
                    "type_udt_catalog",
                    regtype_name.map_or(Value::Null, |_| catalog_name()),
                ),
                (
                    "type_udt_schema",
                    regtype_name.map_or(Value::Null, |_| str_value("pg_catalog")),
                ),
                ("type_udt_name", regtype_name.map_or(Value::Null, str_value)),
                (
                    "routine_body",
                    str_value(if routine.language() == i64::from(SQL_LANGUAGE) {
                        "SQL"
                    } else {
                        "EXTERNAL"
                    }),
                ),
                (
                    "routine_definition",
                    if builtin_owner {
                        str_value(routine.source)
                    } else {
                        Value::Null
                    },
                ),
                ("external_name", Value::Null),
                (
                    "external_language",
                    str_value(if routine.language() == i64::from(SQL_LANGUAGE) {
                        "SQL"
                    } else {
                        "INTERNAL"
                    }),
                ),
                (
                    "is_deterministic",
                    str_value(if routine.volatility == "i" {
                        "YES"
                    } else {
                        "NO"
                    }),
                ),
                ("sql_data_access", str_value("MODIFIES")),
                (
                    "is_null_call",
                    str_value(if routine.strict { "YES" } else { "NO" }),
                ),
                ("schema_level_routine", str_value("YES")),
                ("max_dynamic_result_sets", Value::Int(0)),
                ("is_udt_dependent", str_value("NO")),
            ])
        })
        .collect();
    rows.extend(registered_names().into_iter().map(|name| {
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
        ])
    }));
    for function in catalog.all_sql_functions() {
        let def = &function.def;
        let (routine_schema, routine_name) = split_schema_name(&def.name)?;
        let catalog_oid = user_routine_catalog_oid(&function)?;
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
            ("specific_schema", str_value(routine_schema.clone())),
            (
                "specific_name",
                str_value(format!("{routine_name}_{catalog_oid}")),
            ),
            ("routine_catalog", catalog_name()),
            ("routine_schema", str_value(routine_schema)),
            ("routine_name", str_value(routine_name)),
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
        ]));
    }
    Ok(rows)
}
