//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine OID input borrows declarations and retains only signature-shadowing workspace.

use super::{
    namespace_exists, object_name, routine, schemas, CatalogContext, CatalogReadView,
    ProductionControl, QualifiedNames, SQLError,
};
use crate::catalog::{namespaces, security::schema::SchemaAclPrivilege};
use std::cmp::Ordering;
use uqa_core::{
    memory::{OwnedMap, Produced, ProductionVec},
    RelationIdentity, RelationName,
};

pub(super) fn resolve(
    context: &CatalogContext<'_>,
    input: &str,
    exact: bool,
    control: &ProductionControl<'_>,
) -> Result<i64, SQLError> {
    if exact {
        return exact_signature(context, input, control);
    }
    let names = uqa_sql::parse_regobject_name_with_control(input, control)?
        .ok_or_else(|| routine("42602", format_args!("invalid name syntax"), control))?;
    let (schema, local) = object_name(&names, control)?;
    let catalog = context.catalog_read_view();
    if let Some(schema) = schema {
        if !namespace_exists(&catalog, schema) {
            return Err(routine(
                "3F000",
                format_args!("schema \"{schema}\" does not exist"),
                control,
            ));
        }
    }
    let mut visible = OwnedMap::<Signature, (usize, i64)>::new();
    for (position, schema) in schemas(context, schema, control)?.iter().enumerate() {
        visit(&catalog, schema, local, control, |oid, arguments| {
            if let Some((first, previous)) = visible.get_mut(arguments) {
                if *first == position {
                    *previous = (*previous).min(oid);
                }
                return Ok(());
            }
            let mut signature = ProductionVec::new(*control);
            signature.reserve(arguments.len())?;
            for argument in arguments {
                signature.push_copy(*argument)?;
            }
            let signature = Signature(signature.finish()?);
            if let Some(budget) = control.budget() {
                visible.insert_budgeted(signature, (position, oid), budget)?;
            } else {
                visible.insert(signature, (position, oid));
            }
            Ok(())
        })?;
    }
    let mut values = visible.iter().map(|(_, (_, oid))| *oid);
    match (values.next(), values.next()) {
        (Some(oid), None) => Ok(oid),
        (None, _) => Err(routine(
            "42883",
            format_args!("function \"{input}\" does not exist"),
            control,
        )),
        (Some(_), Some(_)) => Err(routine(
            "42725",
            format_args!("more than one function named \"{input}\""),
            control,
        )),
    }
}

fn exact_signature(
    context: &CatalogContext<'_>,
    input: &str,
    control: &ProductionControl<'_>,
) -> Result<i64, SQLError> {
    let parsed =
        uqa_sql::parse_regprocedure_name_with_control(input, control)?.ok_or_else(|| {
            routine(
                "42602",
                format_args!("invalid name syntax: \"{input}\""),
                control,
            )
        })?;
    let arguments = parsed.argument_types.as_ref().ok_or_else(|| {
        routine(
            "22P02",
            format_args!("expected a left parenthesis"),
            control,
        )
    })?;
    let names = if parsed.names.len() == 3 && parsed.names[0] == uqa_sql::catalog::DATABASE_NAME {
        &parsed.names[1..]
    } else {
        &parsed.names
    };
    let (schema, local) = object_name(names, control)?;
    let mut targets = ProductionVec::new(*control);
    for argument in arguments {
        let oid = super::types::parsed_oid(context, argument, control)?.ok_or_else(|| {
            routine(
                "42704",
                format_args!(
                    "type \"{}\" does not exist",
                    QualifiedNames(&argument.names)
                ),
                control,
            )
        })?;
        targets.push_copy(oid)?;
    }
    let targets = targets.finish()?;
    let catalog = context.catalog_read_view();
    if let Some(schema) = schema {
        let resolution = context
            .session
            .relation_name_resolution_with_control(control)?;
        if namespaces::namespace_schema_exists(&catalog, &resolution.temporary_schema, schema) {
            namespaces::require_schema_privilege_with_control(
                &catalog,
                &resolution.temporary_schema,
                schema,
                &resolution.current_user,
                SchemaAclPrivilege::Usage,
                control,
            )?;
        }
    }
    for schema in schemas(context, schema, control)?.iter() {
        let mut selected: Option<i64> = None;
        visit(&catalog, schema, local, control, |oid, signature| {
            if signature == targets.as_slice() {
                selected = Some(selected.map_or(oid, |previous| previous.min(oid)));
            }
            Ok(())
        })?;
        if let Some(selected) = selected {
            return Ok(selected);
        }
    }
    Err(routine(
        "42883",
        format_args!("function \"{input}\" does not exist"),
        control,
    ))
}

fn visit(
    catalog: &CatalogReadView,
    schema: &str,
    name: &str,
    control: &ProductionControl<'_>,
    mut visitor: impl FnMut(i64, &[i64]) -> Result<(), SQLError>,
) -> Result<(), SQLError> {
    if schema == "pg_catalog" {
        for builtin in crate::catalog::projection::builtin_routines::builtin_routine_identities() {
            control.check()?;
            if builtin.name == name {
                visitor(i64::from(builtin.oid), builtin.argument_types)?;
            }
        }
    }
    for function in catalog
        .snapshot()
        .definitions
        .sql_user_functions
        .values()
        .flatten()
    {
        control.check()?;
        let Some(identity) = RelationIdentity::borrowed_reference(&function.def.name) else {
            return Err(super::formatted(
                format_args!("invalid catalog routine `{}`", function.def.name),
                control,
                SQLError::Internal,
            ));
        };
        if identity.schema.unwrap_or(RelationName::decoded("public"))
            != RelationName::decoded(schema)
            || identity.name != RelationName::decoded(name)
        {
            continue;
        }
        let oid = if let Some(oid) = function.def.catalog_oid {
            i64::from(oid)
        } else if let Some(identity) = function.def.object_id {
            uqa_sql::catalog::oids::stable_object_oid("proc", &identity)
        } else {
            return Err(super::formatted(
                format_args!(
                    "routine `{}` has no catalog object identity",
                    function.def.name
                ),
                control,
                SQLError::Internal,
            ));
        };
        let mut arguments = ProductionVec::new(*control);
        for parameter in function.def.identity_params() {
            arguments.push_copy(
                super::super::type_names::catalog_routine_type_oid_with_control(
                    catalog,
                    &parameter.type_name,
                    control,
                )?,
            )?;
        }
        visitor(oid, &arguments.finish()?)?;
    }
    Ok(())
}

struct Signature(Produced<Vec<i64>>);
impl std::borrow::Borrow<[i64]> for Signature {
    fn borrow(&self) -> &[i64] {
        &self.0
    }
}
impl PartialEq for Signature {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for Signature {}
impl PartialOrd for Signature {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Signature {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.as_slice().cmp(other.0.as_slice())
    }
}
