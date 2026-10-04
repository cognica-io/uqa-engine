//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Virtual `pg_settings` row synthesis: one row for every parameter that `SHOW ALL` lists, as `PostgreSQL`'s `show_all_settings` reports it.

use super::helpers::rows::{bool_value, catalog_array, row, str_value};
use crate::catalog::services::CatalogSession;
use uqa_core::Value;
use uqa_sql::semantics::parameters::definition::ParameterKind;
use uqa_sql::semantics::parameters::setting::ParameterSetting;
use uqa_sql::semantics::parameters::units::ParameterUnit;
use uqa_sql::semantics::parameters::value::listed_enum_values;
use uqa_sql::{ResultRow, SQLError};

pub fn build_pg_settings(session: &dyn CatalogSession) -> Result<Vec<ResultRow>, SQLError> {
    session
        .parameter_settings()
        .iter()
        .map(build_pg_setting_row)
        .collect()
}

fn optional(text: Option<&str>) -> Value {
    text.map_or(Value::Null, str_value)
}

fn build_pg_setting_row(setting: &ParameterSetting) -> Result<ResultRow, SQLError> {
    let definition = setting.definition;
    let (min_val, max_val) = match definition.kind {
        ParameterKind::Integer { min, max, .. } => {
            (str_value(min.to_string()), str_value(max.to_string()))
        }
        ParameterKind::Bool { .. } | ParameterKind::Enum { .. } | ParameterKind::String { .. } => {
            (Value::Null, Value::Null)
        }
    };
    let enumvals = match definition.kind {
        ParameterKind::Enum { options, .. } => catalog_array(
            listed_enum_values(options)
                .into_iter()
                .map(str_value)
                .collect(),
            "runtime parameter enum values",
        )?,
        ParameterKind::Bool { .. }
        | ParameterKind::Integer { .. }
        | ParameterKind::String { .. } => Value::Null,
    };
    Ok(row([
        ("name", str_value(definition.name)),
        ("setting", str_value(&setting.setting)),
        ("unit", optional(definition.unit().map(ParameterUnit::name))),
        ("category", str_value(definition.category)),
        ("short_desc", str_value(definition.short_desc)),
        ("extra_desc", optional(definition.extra_desc)),
        ("context", str_value(definition.context.name())),
        ("vartype", str_value(definition.kind.type_name())),
        ("source", str_value(setting.source)),
        ("min_val", min_val),
        ("max_val", max_val),
        ("enumvals", enumvals),
        ("boot_val", str_value(definition.boot_setting())),
        ("reset_val", str_value(&setting.reset_setting)),
        ("sourcefile", Value::Null),
        ("sourceline", Value::Null),
        ("pending_restart", bool_value(false)),
    ]))
}
