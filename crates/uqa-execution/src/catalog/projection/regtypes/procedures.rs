//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared exact routine lookup for strict regprocedure input and nullable catalog inquiry.

use super::{
    numeric_regobject_oid, object_name, parsed_regtype_oid, regtype_output_catalog,
    NumericRegobjectOid,
};
use crate::catalog::{context::CatalogContext, security::schema::SchemaAclPrivilege};
use uqa_sql::SQLError;

pub(super) fn lookup_regprocedure_oid(
    context: &CatalogContext<'_>,
    name: &str,
) -> Result<Option<i64>, SQLError> {
    match resolve_regprocedure_input_oid(context, name) {
        Ok(oid) => Ok(Some(oid)),
        Err(SQLError::Parse(_)) => Ok(None),
        Err(SQLError::Routine { sqlstate, .. })
            if matches!(
                sqlstate.as_str(),
                "22P02" | "22003" | "42704" | "42883" | "42602" | "54023"
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

pub fn resolve_regprocedure_input_oid(
    context: &CatalogContext<'_>,
    name: &str,
) -> Result<i64, SQLError> {
    match numeric_regobject_oid(name) {
        NumericRegobjectOid::Valid(oid) => return Ok(oid),
        NumericRegobjectOid::InvalidSyntax => {
            return Err(input_error(
                "22P02",
                format!("invalid input syntax for type oid: \"{name}\""),
            ))
        }
        NumericRegobjectOid::OutOfRange => {
            return Err(input_error("22003", "OID out of range".into()))
        }
        NumericRegobjectOid::NotNumeric => {}
    }
    let parsed = uqa_sql::parse_regprocedure_name(name)?
        .ok_or_else(|| input_error("42602", format!("invalid name syntax: \"{name}\"")))?;
    let arguments = parsed
        .argument_types
        .as_ref()
        .ok_or_else(|| input_error("22P02", "expected a left parenthesis".into()))?;
    let names = match parsed.names.as_slice() {
        [database, ..]
            if parsed.names.len() == 3 && database == uqa_sql::catalog::DATABASE_NAME =>
        {
            &parsed.names[1..]
        }
        names => names,
    };
    let (schema, local) = object_name(names)?;
    let catalog = regtype_output_catalog(context)?;
    let argument_oids = arguments
        .iter()
        .map(|argument| {
            parsed_regtype_oid(context, &catalog, argument)?.ok_or_else(|| {
                input_error(
                    "42704",
                    format!("type \"{}\" does not exist", argument.names.join(".")),
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let find_in_schema = |schema: &str| {
        let namespace_oid = catalog
            .namespaces
            .iter()
            .find_map(|(oid, name)| (name == schema).then_some(*oid))?;
        catalog.procs.iter().find_map(|(oid, entry)| {
            (entry.namespace_oid == namespace_oid
                && entry.name == local
                && entry.argument_types == argument_oids)
                .then_some(*oid)
        })
    };
    let oid = if let Some(schema) = schema {
        if context.schema_security_for_privilege(schema).is_some() {
            context.require_schema_privilege(
                schema,
                &context.current_role(),
                SchemaAclPrivilege::Usage,
            )?;
        }
        find_in_schema(schema)
    } else {
        context
            .current_schema_names(true)?
            .iter()
            .find_map(|schema| find_in_schema(schema))
    };
    oid.ok_or_else(|| input_error("42883", format!("function \"{name}\" does not exist")))
}

fn input_error(sqlstate: &str, message: String) -> SQLError {
    SQLError::Routine {
        sqlstate: sqlstate.into(),
        message,
    }
}
