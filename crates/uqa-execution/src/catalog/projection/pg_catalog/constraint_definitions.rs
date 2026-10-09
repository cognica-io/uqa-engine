//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `pg_get_constraintdef` and the domain rows of `pg_constraint`, as `pg_get_constraintdef_worker` prints definitions and `domainAddCheckConstraint` records them.

use std::fmt::Write;

use uqa_core::Value;
use uqa_sql::ast::{ForeignKeyAction, ForeignKeyMatch};
use uqa_sql::expr::quote_ident;
use uqa_sql::{ResultRow, SQLError};

use crate::catalog::context::CatalogContext;
use crate::catalog::{CatalogReadView, RelationNameResolution};

use super::super::helpers::constraints::{
    constraint_catalog_rows, ConstraintCatalogKind, ConstraintCatalogRow,
};
use super::super::helpers::oids::namespace_oid;
use super::super::helpers::rows::{bool_value, int_value, row, str_value};
use super::constraints::{constraint_index_oid, constraint_row_oid};

pub fn pg_get_constraintdef_value(
    context: &CatalogContext<'_>,
    arguments: &[Value],
) -> Result<Value, SQLError> {
    let (oid, pretty) = match arguments {
        [oid] => (oid, &Value::Bool(false)),
        [oid, pretty] => (oid, pretty),
        _ => {
            return Err(SQLError::BadArity {
                name: "pg_get_constraintdef".into(),
                expected: "1 or 2".into(),
                actual: arguments.len(),
            })
        }
    };
    let (Value::Int(oid), Value::Bool(pretty)) = (oid, pretty) else {
        if matches!(oid, Value::Null) || matches!(pretty, Value::Null) {
            return Ok(Value::Null);
        }
        return Err(SQLError::TypeMismatch(
            "pg_get_constraintdef requires oid and boolean arguments".into(),
        ));
    };
    let catalog = context.catalog_read_view();
    let resolution = context.session_execution_view().relation_name_resolution();
    if let Some(constraint) = constraint_catalog_rows(&catalog, &resolution)?
        .into_iter()
        .find(|constraint| constraint_row_oid(constraint) == *oid)
    {
        return relation_constraint_definition(
            Some(&crate::catalog::projection::CatalogOutput(*context)),
            &catalog,
            &resolution,
            &constraint,
            *pretty,
        )
        .map(Value::Str);
    }
    domain_constraint_definition(
        Some(&crate::catalog::projection::CatalogOutput(*context)),
        &catalog,
        &resolution,
        *oid,
        *pretty,
    )
    .map(|definition| definition.map_or(Value::Null, Value::Str))
}

fn relation_constraint_definition(
    output: Option<&dyn uqa_sql::expr::EngineHook>,
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    constraint: &ConstraintCatalogRow,
    pretty: bool,
) -> Result<String, SQLError> {
    let mut definition =
        match constraint.kind {
            ConstraintCatalogKind::NotNull => {
                let column = constraint.columns.first().ok_or_else(|| {
                    SQLError::Internal("NOT NULL constraint has no column".into())
                })?;
                format!("NOT NULL {}", quote_ident(&column.name))
            }
            ConstraintCatalogKind::Check => {
                let expression = constraint.expression.as_ref().ok_or_else(|| {
                    SQLError::Internal("CHECK constraint has no expression".into())
                })?;
                format!(
                    "CHECK ({})",
                    super::super::view_definition::stored_expression_definition(
                        output, catalog, resolution, expression, pretty,
                    )?
                )
            }
            ConstraintCatalogKind::PrimaryKey | ConstraintCatalogKind::Unique { .. } => {
                key_constraint_definition(catalog, resolution, constraint)?
            }
            ConstraintCatalogKind::ForeignKey => {
                foreign_key_definition(catalog, resolution, constraint)?
            }
        };
    if constraint.state.no_inherit()
        && matches!(
            constraint.kind,
            ConstraintCatalogKind::NotNull | ConstraintCatalogKind::Check
        )
    {
        definition.push_str(" NO INHERIT");
    }
    if constraint.state.deferrable() {
        definition.push_str(" DEFERRABLE");
    }
    if constraint.state.initially_deferred() {
        definition.push_str(" INITIALLY DEFERRED");
    }
    // Validation is irrelevant for a constraint that is not enforced.
    if !constraint.state.enforced() {
        definition.push_str(" NOT ENFORCED");
    } else if !constraint.state.validated() {
        definition.push_str(" NOT VALID");
    }
    Ok(definition)
}

/// `PRIMARY KEY` or `UNIQUE` with its key columns and the non-key columns its index includes.
fn key_constraint_definition(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    constraint: &ConstraintCatalogRow,
) -> Result<String, SQLError> {
    let mut definition = if constraint.kind == ConstraintCatalogKind::PrimaryKey {
        "PRIMARY KEY ".to_string()
    } else if constraint.kind.nulls_distinct() == Some(false) {
        "UNIQUE NULLS NOT DISTINCT ".to_string()
    } else {
        "UNIQUE ".to_string()
    };
    let mut keys = constraint
        .columns
        .iter()
        .map(|column| quote_ident(&column.name))
        .collect::<Vec<_>>();
    if constraint.period {
        if let Some(last) = keys.last_mut() {
            last.push_str(" WITHOUT OVERLAPS");
        }
    }
    write!(definition, "({})", keys.join(", ")).expect("writing to a String cannot fail");
    let indexes = super::catalog_index_relations(catalog, resolution)?;
    let index_oid = constraint_index_oid(constraint, indexes);
    if let Some(index) = indexes.iter().find(|index| index.oid() == index_oid) {
        let included = &index.definition.included_columns;
        if !included.is_empty() {
            let included = included
                .iter()
                .map(|name| quote_ident(name))
                .collect::<Vec<_>>();
            write!(definition, " INCLUDE ({})", included.join(", "))
                .expect("writing to a String cannot fail");
        }
    }
    Ok(definition)
}

/// `FOREIGN KEY` with the referenced relation named as the search path finds it, its match type and its referential actions.
fn foreign_key_definition(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    constraint: &ConstraintCatalogRow,
) -> Result<String, SQLError> {
    let foreign_key = constraint
        .foreign_key
        .as_ref()
        .ok_or_else(|| SQLError::Internal("FOREIGN KEY constraint has no referenced key".into()))?;
    let referenced_name = format!(
        "{}.{}",
        quote_ident(&foreign_key.schema),
        quote_ident(&foreign_key.table)
    );
    let mut bound = resolution.clone();
    bound.set_lookup_mode(crate::catalog::RelationLookupMode::Bound);
    let referenced = catalog
        .table(&bound, &referenced_name)?
        .ok_or_else(|| SQLError::UnknownTable(referenced_name.clone()))?;
    let mut remote = foreign_key
        .column_ordinals
        .iter()
        .map(|ordinal| {
            uqa_sql::catalog::relation_attributes::column_by_number(&referenced.columns, *ordinal)
                .map(|column| quote_ident(&column.name))
                .ok_or_else(|| SQLError::Internal("referenced key column disappeared".into()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut local = constraint
        .columns
        .iter()
        .map(|column| quote_ident(&column.name))
        .collect::<Vec<_>>();
    if constraint.period {
        for keys in [&mut local, &mut remote] {
            if let Some(last) = keys.last_mut() {
                *last = format!("PERIOD {last}");
            }
        }
    }
    let mut definition = format!(
        "FOREIGN KEY ({}) REFERENCES {}({})",
        local.join(", "),
        relation_display_name(catalog, resolution, &foreign_key.schema, &foreign_key.table)?,
        remote.join(", ")
    );
    if foreign_key.match_type == ForeignKeyMatch::Full {
        definition.push_str(" MATCH FULL");
    }
    if let Some(action) = foreign_key_action(foreign_key.on_update) {
        write!(definition, " ON UPDATE {action}").expect("writing to a String cannot fail");
    }
    if let Some(action) = foreign_key_action(foreign_key.on_delete) {
        write!(definition, " ON DELETE {action}").expect("writing to a String cannot fail");
    }
    Ok(definition)
}

const fn foreign_key_action(action: ForeignKeyAction) -> Option<&'static str> {
    match action {
        ForeignKeyAction::NoAction => None,
        ForeignKeyAction::Restrict => Some("RESTRICT"),
        ForeignKeyAction::Cascade => Some("CASCADE"),
        ForeignKeyAction::SetNull => Some("SET NULL"),
        ForeignKeyAction::SetDefault => Some("SET DEFAULT"),
    }
}

/// `generate_relation_name`: a relation the search path finds by its bare name prints unqualified.
fn relation_display_name(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    schema: &str,
    table: &str,
) -> Result<String, SQLError> {
    let canonical = format!("{}.{}", quote_ident(schema), quote_ident(table));
    let visible = catalog
        .relation_kind_resolution(resolution, &quote_ident(table))?
        .into_found()
        .is_some_and(|(found, _)| {
            found == canonical
                || uqa_core::RelationIdentity::from_legacy_name(&found)
                    .is_ok_and(|identity| identity.schema == schema && identity.name == table)
        });
    Ok(if visible {
        quote_ident(table)
    } else {
        canonical
    })
}

fn domain_constraint_definition(
    output: Option<&dyn uqa_sql::expr::EngineHook>,
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    oid: i64,
    pretty: bool,
) -> Result<Option<String>, SQLError> {
    for domain in catalog.domains() {
        if domain
            .definition
            .not_null
            .as_ref()
            .and_then(|constraint| constraint.catalog_identity)
            .is_some_and(|identity| identity.oid == oid)
        {
            return Ok(Some("NOT NULL".into()));
        }
        if let Some(check) = domain.definition.checks.iter().find(|check| {
            check
                .catalog_identity
                .is_some_and(|identity| identity.oid == oid)
        }) {
            return super::super::view_definition::stored_domain_expression_definition(
                output,
                catalog,
                resolution,
                &check.expression,
                pretty,
            )
            .map(|expression| {
                let validation = if check.validated { "" } else { " NOT VALID" };
                Some(format!("CHECK ({expression}){validation}"))
            });
        }
    }
    Ok(None)
}

/// `pg_constraint` rows of domain constraints: they constrain a type rather than a relation.
pub fn domain_constraint_rows(catalog: &CatalogReadView) -> Result<Vec<ResultRow>, SQLError> {
    let mut rows = Vec::new();
    for domain in catalog.domains() {
        let namespace = namespace_oid(catalog, &domain.identity.schema);
        let not_null = domain.definition.not_null.iter().map(|constraint| {
            (
                constraint.name.as_deref(),
                constraint.catalog_identity,
                "n",
                true,
            )
        });
        let checks = domain.definition.checks.iter().map(|constraint| {
            (
                constraint.name.as_deref(),
                constraint.catalog_identity,
                "c",
                constraint.validated,
            )
        });
        for (name, identity, kind, validated) in not_null.chain(checks) {
            let (Some(name), Some(identity)) = (name, identity) else {
                return Err(SQLError::Internal(format!(
                    "constraint of domain {} has no catalog identity",
                    domain.identity.qualified_name()
                )));
            };
            rows.push(row([
                ("oid", int_value(identity.oid)),
                ("conname", str_value(name.to_string())),
                ("connamespace", int_value(namespace)),
                ("contype", str_value(kind)),
                ("condeferrable", bool_value(false)),
                ("condeferred", bool_value(false)),
                ("conenforced", bool_value(true)),
                ("convalidated", bool_value(validated)),
                ("conrelid", int_value(0)),
                ("contypid", int_value(i64::from(domain.oid))),
                ("conindid", int_value(0)),
                ("conparentid", int_value(0)),
                ("confrelid", int_value(0)),
                ("confupdtype", str_value(" ")),
                ("confdeltype", str_value(" ")),
                ("confmatchtype", str_value(" ")),
                ("conislocal", bool_value(true)),
                ("coninhcount", int_value(0)),
                ("connoinherit", bool_value(false)),
                ("conperiod", bool_value(false)),
                ("conkey", Value::Null),
                ("confkey", Value::Null),
                ("conpfeqop", Value::Null),
                ("conppeqop", Value::Null),
                ("conffeqop", Value::Null),
                ("conexclop", Value::Null),
                ("conbin", Value::Null),
            ]));
        }
    }
    Ok(rows)
}
