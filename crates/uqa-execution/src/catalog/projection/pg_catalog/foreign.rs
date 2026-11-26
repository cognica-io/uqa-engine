//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Foreign catalog projections use the retained object references and written option order.

use super::super::helpers::{
    acl::object_acl_items,
    rows::{catalog_array, int_value, row, str_value},
};
use crate::catalog::CatalogReadView;
use uqa_core::Value;
use uqa_sql::{catalog::foreign_wrapper::ForeignWrapperHandler, ResultRow, SQLError};

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
                            .map(|(name, value)| Ok((name.as_str(), value.as_str()))),
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
                    option_array(uqa_sql::schema::foreign_servers::ordered_option_pairs(
                        &server.options,
                        metadata.option_order.as_deref(),
                    ))?,
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
        .filter(|table| table.server_reference.is_some())
        .map(|table| {
            Ok(row([
                (
                    "ftrelid",
                    int_value(i64::from(table.relation_oids().relation)),
                ),
                ("ftserver", int_value(i64::from(table.server_oid()?))),
                (
                    "ftoptions",
                    option_array(uqa_sql::schema::foreign_servers::ordered_option_pairs(
                        &table.options,
                        Some(&table.option_order),
                    ))?,
                ),
            ]))
        })
        .collect()
}

fn option_array<'a>(
    options: impl Iterator<Item = Result<(&'a str, &'a str), SQLError>>,
) -> Result<Value, SQLError> {
    let values: Vec<_> = options
        .map(|pair| pair.map(|(name, value)| Value::Str(format!("{name}={value}"))))
        .collect::<Result<_, _>>()?;
    if values.is_empty() {
        Ok(Value::Null)
    } else {
        catalog_array(values, "foreign catalog options")
    }
}
