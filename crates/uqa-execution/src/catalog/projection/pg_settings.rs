//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Virtual `pg_settings` row synthesis.

use super::helpers::rows::{bool_value, catalog_array, row, str_value};
use crate::catalog::services::CatalogSession;
use uqa_core::Value;
use uqa_sql::{ResultRow, SQLError};

pub fn build_pg_settings(session: &dyn CatalogSession) -> Result<Vec<ResultRow>, SQLError> {
    let settings = [
        ("server_version", "Version and compatibility"),
        ("server_encoding", "Client connection defaults"),
        ("client_encoding", "Client connection defaults"),
        ("DateStyle", "Locale and formatting"),
        ("TimeZone", "Locale and formatting"),
        ("work_mem", "Resource usage"),
        ("plan_cache_mode", "Query Tuning / Other Planner Options"),
        ("session_replication_role", "Replication"),
        ("plpgsql.check_asserts", "Customized Options"),
        ("search_path", "Client connection defaults"),
        (
            "default_transaction_isolation",
            "Client connection defaults",
        ),
        (
            "default_transaction_read_only",
            "Client connection defaults",
        ),
        (
            "default_transaction_deferrable",
            "Client connection defaults",
        ),
        ("transaction_isolation", "Client connection defaults"),
        ("transaction_read_only", "Client connection defaults"),
        ("transaction_deferrable", "Client connection defaults"),
    ];
    settings
        .into_iter()
        .map(|(name, category)| {
            let setting = session.show_variable(name)?;
            build_pg_setting_row(
                name,
                category,
                &setting,
                session.runtime_parameter_source(name),
            )
        })
        .collect()
}

fn build_pg_setting_row(
    name: &str,
    category: &str,
    setting: &str,
    source: &str,
) -> Result<ResultRow, SQLError> {
    let replication_role = name == "session_replication_role";
    let plan_cache_mode = name == "plan_cache_mode";
    let check_asserts = name == "plpgsql.check_asserts";
    let enumvals = if replication_role || plan_cache_mode {
        catalog_array(
            if plan_cache_mode {
                ["auto", "force_generic_plan", "force_custom_plan"]
            } else {
                ["origin", "replica", "local"]
            }
            .into_iter()
            .map(str_value)
            .collect(),
            "runtime parameter enum values",
        )?
    } else {
        Value::Null
    };
    Ok(row([
        ("name", str_value(name)),
        ("setting", str_value(setting)),
        ("unit", Value::Null),
        ("category", str_value(category)),
        (
            "short_desc",
            str_value(if plan_cache_mode {
                "Controls the planner's selection of custom or generic plan."
            } else if check_asserts {
                "Perform checks given in ASSERT statements."
            } else {
                name
            }),
        ),
        (
            "extra_desc",
            if plan_cache_mode {
                str_value("Prepared statements can have custom and generic plans, and the planner will attempt to choose which is better.  This can be set to override the default behavior.")
            } else {
                Value::Null
            },
        ),
        (
            "context",
            str_value(if replication_role {
                "superuser"
            } else {
                "user"
            }),
        ),
        (
            "vartype",
            str_value(if replication_role || plan_cache_mode {
                "enum"
            } else if check_asserts {
                "bool"
            } else {
                "string"
            }),
        ),
        ("source", str_value(source)),
        ("min_val", Value::Null),
        ("max_val", Value::Null),
        ("enumvals", enumvals),
        (
            "boot_val",
            str_value(if plan_cache_mode {
                "auto"
            } else if replication_role {
                "origin"
            } else if check_asserts {
                "on"
            } else {
                setting
            }),
        ),
        (
            "reset_val",
            str_value(if plan_cache_mode {
                "auto"
            } else if replication_role {
                "origin"
            } else if check_asserts {
                "on"
            } else {
                setting
            }),
        ),
        ("sourcefile", Value::Null),
        ("sourceline", Value::Null),
        ("pending_restart", bool_value(false)),
    ]))
}
