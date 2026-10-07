//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Foreign catalog projections use the retained object references and written option order.

use super::super::helpers::{
    acl::object_acl_items,
    rows::{catalog_array, catalog_ordinal, int_value, row, str_value},
};
use super::{
    attributes::{attribute_column, pg_attribute_row},
    relations::pg_class_catalog_row,
};
use crate::catalog::CatalogReadView;
use std::collections::BTreeMap;
use uqa_core::{catalog_role::RoleIdentity, RelationIdentity, Value};
use uqa_sql::{
    catalog::{
        foreign_relations::FOREIGN_CATALOGS, foreign_wrapper::ForeignWrapperHandler,
        relation_oids::RelationCatalogOids,
    },
    ResultRow, SQLError,
};

pub fn wrappers(catalog: &CatalogReadView) -> Result<Vec<ResultRow>, SQLError> {
    let definitions = &catalog.snapshot().definitions;
    definitions
        .foreign_wrappers
        .values()
        .map(|wrapper| {
            let (handler, acl) = match wrapper.handler {
                ForeignWrapperHandler::None => (0, Value::Null),
                ForeignWrapperHandler::Function(ref function) => {
                    (i64::from(function.oid), Value::Null)
                }
                ForeignWrapperHandler::Native(native) if wrapper.identity != native.reference() => {
                    (native.handler_oid(), Value::Null)
                }
                ForeignWrapperHandler::Native(native) => {
                    let acl = [
                        uqa_sql::ast::ObjectAclEntry {
                            role: Some(wrapper.owner),
                            grantor: wrapper.owner,
                            grant_option: false,
                        },
                        uqa_sql::ast::ObjectAclEntry {
                            role: None,
                            grantor: wrapper.owner,
                            grant_option: false,
                        },
                    ];
                    (
                        native.handler_oid(),
                        catalog_array(
                            object_acl_items(
                                &definitions.roles,
                                &acl,
                                'U',
                                "foreign-data wrapper",
                            )?,
                            "pg_foreign_data_wrapper.fdwacl",
                        )?,
                    )
                }
            };
            Ok(row([
                ("oid", int_value(i64::from(wrapper.identity.oid))),
                ("fdwname", str_value(&wrapper.name)),
                ("fdwowner", int_value(wrapper.owner.oid)),
                ("fdwhandler", int_value(handler)),
                (
                    "fdwvalidator",
                    int_value(
                        wrapper
                            .validator
                            .as_ref()
                            .map_or(0, |function| i64::from(function.oid)),
                    ),
                ),
                ("fdwacl", acl),
                (
                    "fdwoptions",
                    option_array(
                        wrapper
                            .options
                            .iter()
                            .map(|(name, value)| (name.as_str(), value.as_str())),
                    )?,
                ),
            ]))
        })
        .collect()
}

pub fn servers(catalog: &CatalogReadView) -> Result<Vec<ResultRow>, SQLError> {
    catalog
        .snapshot()
        .definitions
        .foreign_servers
        .values()
        .map(|server| {
            let metadata = &server.metadata;
            let wrapper = metadata.wrapper_reference.ok_or_else(|| {
                SQLError::Internal(format!(
                    "foreign server `{}` has no wrapper identity",
                    server.name
                ))
            })?;
            Ok(row([
                ("oid", int_value(i64::from(metadata.oid))),
                ("srvname", str_value(&server.name)),
                ("srvowner", int_value(metadata.owner.oid)),
                ("srvfdw", int_value(i64::from(wrapper.oid))),
                (
                    "srvtype",
                    metadata
                        .server_type
                        .as_deref()
                        .map_or(Value::Null, str_value),
                ),
                (
                    "srvversion",
                    metadata.version.as_deref().map_or(Value::Null, str_value),
                ),
                ("srvacl", Value::Null),
                (
                    "srvoptions",
                    ordered_options(&server.options, metadata.option_order.as_deref())?,
                ),
            ]))
        })
        .collect()
}

pub fn tables(catalog: &CatalogReadView) -> Result<Vec<ResultRow>, SQLError> {
    catalog
        .snapshot()
        .definitions
        .foreign_tables
        .values()
        .map(|table| {
            Ok(row([
                (
                    "ftrelid",
                    int_value(i64::from(table.relation_oids().relation)),
                ),
                ("ftserver", int_value(i64::from(table.server_oid()?))),
                (
                    "ftoptions",
                    ordered_options(&table.options, Some(&table.option_order))?,
                ),
            ]))
        })
        .collect()
}

fn option_array<'a>(options: impl Iterator<Item = (&'a str, &'a str)>) -> Result<Value, SQLError> {
    let values: Vec<_> = options
        .map(|(name, value)| Value::Str(format!("{name}={value}")))
        .collect();
    if values.is_empty() {
        Ok(Value::Null)
    } else {
        catalog_array(values, "foreign catalog options")
    }
}

fn ordered_options(
    options: &BTreeMap<String, String>,
    order: Option<&[String]>,
) -> Result<Value, SQLError> {
    if let Some(order) = order {
        let pairs = order
            .iter()
            .map(|name| {
                options
                    .get(name)
                    .map(|value| (name.as_str(), value.as_str()))
                    .ok_or_else(|| {
                        SQLError::Internal("foreign option order references a missing key".into())
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        option_array(pairs.into_iter())
    } else {
        option_array(
            options
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str())),
        )
    }
}

pub fn class_rows(catalog: &CatalogReadView) -> Vec<ResultRow> {
    FOREIGN_CATALOGS
        .iter()
        .map(|descriptor| {
            let relation = descriptor.relation;
            pg_class_catalog_row(
                catalog,
                relation.oid(),
                i64::from(descriptor.row_type),
                relation.namespace(),
                relation.name(),
                "r",
                i64::try_from(relation.schema().len()).expect("foreign catalog column count"),
                0.0,
                true,
            )
        })
        .collect()
}

pub fn attribute_rows() -> Result<Vec<ResultRow>, SQLError> {
    let mut rows = Vec::new();
    for descriptor in FOREIGN_CATALOGS {
        for (index, (name, ty)) in descriptor.relation.schema().into_iter().enumerate() {
            rows.push(pg_attribute_row(
                descriptor.relation.oid(),
                catalog_ordinal(index, "foreign catalog attribute")?,
                &attribute_column(&name, ty, index < descriptor.required_columns),
            ));
        }
    }
    Ok(rows)
}

pub fn type_rows(catalog: &CatalogReadView) -> Vec<ResultRow> {
    let mut rows = Vec::new();
    for descriptor in FOREIGN_CATALOGS {
        let relation = descriptor.relation;
        let array_name = format!("_{}", relation.name());
        super::row_types::append_rows(
            &mut rows,
            catalog,
            &RelationIdentity::new(relation.namespace(), relation.name()),
            RelationCatalogOids {
                relation: u32::try_from(relation.oid()).expect("foreign catalog relation OID"),
                row_type: Some(descriptor.row_type),
                array_type: Some(descriptor.array_type),
                rule: None,
            },
            RoleIdentity::BOOTSTRAP,
            Some(&array_name),
        );
    }
    rows
}
