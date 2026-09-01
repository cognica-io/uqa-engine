//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Virtual `pg_settings` row synthesis.

use super::helpers::rows::{bool_value, catalog_array, row, str_value};
use crate::engine_capabilities::SessionExecutionView;
use uqa_core::Value;
use uqa_sql::{ResultRow, SQLError};

pub(super) fn build_pg_settings(
    session: SessionExecutionView<'_>,
) -> Result<Vec<ResultRow>, SQLError> {
    let settings = [
        ("server_version", "Version and compatibility"),
        ("server_encoding", "Client connection defaults"),
        ("client_encoding", "Client connection defaults"),
        ("DateStyle", "Locale and formatting"),
        ("TimeZone", "Locale and formatting"),
        ("work_mem", "Resource usage"),
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
            build_pg_setting_row(name, category, &setting)
        })
        .collect()
}

fn build_pg_setting_row(name: &str, category: &str, setting: &str) -> Result<ResultRow, SQLError> {
    let replication_role = name == "session_replication_role";
    let check_asserts = name == "plpgsql.check_asserts";
    let enumvals = if replication_role {
        catalog_array(
            ["origin", "replica", "local"]
                .into_iter()
                .map(str_value)
                .collect(),
            "session_replication_role enum values",
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
            str_value(if check_asserts {
                "Perform checks given in ASSERT statements."
            } else {
                name
            }),
        ),
        ("extra_desc", Value::Null),
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
            str_value(if replication_role {
                "enum"
            } else if check_asserts {
                "bool"
            } else {
                "string"
            }),
        ),
        ("source", str_value("default")),
        ("min_val", Value::Null),
        ("max_val", Value::Null),
        ("enumvals", enumvals),
        (
            "boot_val",
            str_value(if replication_role {
                "origin"
            } else if check_asserts {
                "on"
            } else {
                setting
            }),
        ),
        (
            "reset_val",
            str_value(if replication_role {
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
