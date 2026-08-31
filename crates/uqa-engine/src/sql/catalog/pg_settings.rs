//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Virtual `pg_settings` row synthesis.

use super::helpers::{bool_value, catalog_array, row, str_value};
use super::{ResultRow, SQLError, Value};
use crate::engine_capabilities::SessionExecutionView;

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
            let replication_role = name == "session_replication_role";
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
                ("setting", str_value(setting.as_str())),
                ("unit", Value::Null),
                ("category", str_value(category)),
                ("short_desc", str_value(name)),
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
                    str_value(if replication_role { "enum" } else { "string" }),
                ),
                ("source", str_value("default")),
                ("min_val", Value::Null),
                ("max_val", Value::Null),
                ("enumvals", enumvals),
                (
                    "boot_val",
                    str_value(if replication_role {
                        "origin"
                    } else {
                        setting.as_str()
                    }),
                ),
                (
                    "reset_val",
                    str_value(if replication_role { "origin" } else { &setting }),
                ),
                ("sourcefile", Value::Null),
                ("sourceline", Value::Null),
                ("pending_restart", bool_value(false)),
            ]))
        })
        .collect()
}
