//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Constraint catalog projection.

use uqa_core::Value;
use uqa_sql::{ResultRow, SQLError};

use crate::engine_capabilities::{CatalogReadView, RelationNameResolution};

use super::super::helpers::constraints::constraint_catalog_rows;
use super::super::helpers::oids::{schema_oid, stable_oid};
use super::super::helpers::rows::{bool_value, catalog_array, int_value, row, str_value};
use super::table_relation_oid_from;

#[expect(
    clippy::too_many_lines,
    reason = "preserves catalog column and OID order"
)]
pub(in crate::sql::catalog) fn build_pg_constraint(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
) -> Result<Vec<ResultRow>, SQLError> {
    let mut rows = constraint_catalog_rows(catalog, resolution)?
        .into_iter()
        .map(|constraint| -> Result<ResultRow, SQLError> {
            let foreign_key = constraint.foreign_key.as_ref();
            let constrained_key: Vec<i64> = constraint
                .columns
                .iter()
                .map(|column| column.table_ordinal)
                .collect();
            let constrained_key = if constrained_key.is_empty() {
                Value::Null
            } else {
                catalog_array(
                    constrained_key.into_iter().map(Value::Int).collect(),
                    "pg_constraint.conkey",
                )?
            };
            let referenced_key = match foreign_key {
                Some(foreign_key) => catalog_array(
                    foreign_key
                        .column_ordinals
                        .iter()
                        .copied()
                        .map(Value::Int)
                        .collect(),
                    "pg_constraint.confkey",
                )?,
                None => Value::Null,
            };
            let constrained_relation_oid = table_relation_oid_from(
                catalog,
                resolution,
                &format!(
                    "{}.{}",
                    uqa_sql::expr::quote_ident(&constraint.schema),
                    uqa_sql::expr::quote_ident(&constraint.table)
                ),
            )?;
            let referenced_relation_oid = match foreign_key {
                Some(foreign_key) => table_relation_oid_from(
                    catalog,
                    resolution,
                    &format!(
                        "{}.{}",
                        uqa_sql::expr::quote_ident(&foreign_key.schema),
                        uqa_sql::expr::quote_ident(&foreign_key.table)
                    ),
                )?,
                None => 0,
            };
            Ok(row([
                (
                    "oid",
                    int_value(stable_oid(
                        "constraint",
                        &format!(
                            "{}.{}.{}",
                            constraint.schema, constraint.table, constraint.name
                        ),
                    )),
                ),
                ("conname", str_value(constraint.name)),
                ("connamespace", int_value(schema_oid(&constraint.schema))),
                ("contype", str_value(constraint.kind.pg_type())),
                ("condeferrable", bool_value(constraint.state.deferrable())),
                (
                    "condeferred",
                    bool_value(constraint.state.initially_deferred()),
                ),
                ("conenforced", bool_value(constraint.state.enforced())),
                ("convalidated", bool_value(constraint.state.validated())),
                ("conrelid", int_value(constrained_relation_oid)),
                ("contypid", int_value(0)),
                ("conindid", int_value(0)),
                ("conparentid", int_value(0)),
                ("confrelid", int_value(referenced_relation_oid)),
                (
                    "confupdtype",
                    str_value(foreign_key.map_or(" ", |foreign_key| {
                        foreign_key_action_code(foreign_key.on_update)
                    })),
                ),
                (
                    "confdeltype",
                    str_value(foreign_key.map_or(" ", |foreign_key| {
                        foreign_key_action_code(foreign_key.on_delete)
                    })),
                ),
                (
                    "confmatchtype",
                    str_value(foreign_key.map_or(" ", |foreign_key| {
                        foreign_key_match_code(foreign_key.match_type)
                    })),
                ),
                ("conislocal", bool_value(true)),
                ("coninhcount", int_value(0)),
                ("connoinherit", bool_value(constraint.state.no_inherit())),
                ("conperiod", bool_value(constraint.period)),
                ("conkey", constrained_key),
                ("confkey", referenced_key),
                ("conpfeqop", Value::Null),
                ("conppeqop", Value::Null),
                ("conffeqop", Value::Null),
                ("conexclop", Value::Null),
                ("conbin", Value::Null),
            ]))
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    rows.extend(super::super::events::build_trigger_constraints(
        catalog, resolution,
    )?);
    Ok(rows)
}

const fn foreign_key_action_code(action: uqa_sql::ast::ForeignKeyAction) -> &'static str {
    match action {
        uqa_sql::ast::ForeignKeyAction::NoAction => "a",
        uqa_sql::ast::ForeignKeyAction::Restrict => "r",
        uqa_sql::ast::ForeignKeyAction::Cascade => "c",
        uqa_sql::ast::ForeignKeyAction::SetNull => "n",
        uqa_sql::ast::ForeignKeyAction::SetDefault => "d",
    }
}

const fn foreign_key_match_code(match_type: uqa_sql::ast::ForeignKeyMatch) -> &'static str {
    match match_type {
        uqa_sql::ast::ForeignKeyMatch::Simple => "s",
        uqa_sql::ast::ForeignKeyMatch::Full => "f",
    }
}
