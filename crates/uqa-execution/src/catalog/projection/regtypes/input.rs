//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! OID input reads selected retained identities and admits request-owned parsing and lookup workspace.

use super::{CatalogContext, NumericRegobjectOid};
use crate::catalog::{namespaces, CatalogReadView};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};
use uqa_sql::{ColumnType, SQLError};

mod relations;
pub use relations::resolved_relation_dependency_with_control;
mod routines;
mod types;

pub fn resolve_oid_alias_input_with_control(
    context: &CatalogContext<'_>,
    ty: &ColumnType,
    name: &str,
    control: &ProductionControl<'_>,
) -> Result<Option<i64>, SQLError> {
    control.check()?;
    if let Some(oid) = numeric(name, control)? {
        return Ok(Some(oid));
    }
    match ty {
        ColumnType::Regclass => relations::resolve(context, name, control).map(Some),
        ColumnType::Regtype => types::resolve(context, name, control),
        ColumnType::Regproc => routines::resolve(context, name, false, control).map(Some),
        ColumnType::Regprocedure => routines::resolve(context, name, true, control).map(Some),
        ColumnType::Regcollation => {
            super::collations::resolve_regcollation_name_with_control(context, name, control)
                .map(Some)
        }
        ColumnType::Regnamespace | ColumnType::Regrole => {
            let names = uqa_sql::parse_regobject_name_with_control(name, control)?
                .ok_or_else(|| routine("42602", format_args!("invalid name syntax"), control))?;
            let [local] = names.as_slice() else {
                return Err(routine(
                    "42602",
                    format_args!("invalid name syntax"),
                    control,
                ));
            };
            let catalog = context.catalog_read_view();
            if matches!(ty, ColumnType::Regrole) {
                for role in catalog.roles() {
                    control.check()?;
                    if role.name == *local {
                        return Ok(Some(role.oid));
                    }
                }
                Err(routine(
                    "42704",
                    format_args!("role \"{}\" does not exist", EscapedName(local)),
                    control,
                ))
            } else if namespace_exists(&catalog, local) {
                Ok(Some(super::super::helpers::oids::namespace_oid(
                    &catalog, local,
                )))
            } else {
                Err(routine(
                    "3F000",
                    format_args!("schema \"{}\" does not exist", EscapedName(local)),
                    control,
                ))
            }
        }
        _ => Err(SQLError::Internal(
            "expected an OID alias input type".into(),
        )),
    }
}

fn numeric(input: &str, control: &ProductionControl<'_>) -> Result<Option<i64>, SQLError> {
    control.check()?;
    let value = super::numeric_regobject_oid(input);
    control.check()?;
    match value {
        NumericRegobjectOid::Valid(oid) => Ok(Some(oid)),
        NumericRegobjectOid::InvalidSyntax => Err(routine(
            "22P02",
            format_args!("invalid input syntax for type oid: \"{input}\""),
            control,
        )),
        NumericRegobjectOid::OutOfRange => Err(routine(
            "22003",
            format_args!("value \"{input}\" is out of range for type oid"),
            control,
        )),
        NumericRegobjectOid::NotNumeric => Ok(None),
    }
}

fn namespace_exists(catalog: &CatalogReadView, name: &str) -> bool {
    matches!(name, "pg_catalog" | "information_schema" | "ag_catalog")
        || catalog.snapshot().definitions.schemas.contains_key(name)
        || catalog.contains_graph(name)
        || catalog
            .snapshot()
            .temporary_namespace
            .as_ref()
            .is_some_and(|namespace| namespace.namespace_oid(name).is_some())
}

fn schemas(
    context: &CatalogContext<'_>,
    qualified: Option<&str>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<String>>, SQLError> {
    if let Some(schema) = qualified {
        let mut output = ProductionVec::new(*control);
        output.push_produced(control.copy_text(schema)?)?;
        return Ok(output.finish()?);
    }
    let catalog = context.catalog.refreshed_catalog_snapshot()?;
    let resolution = context
        .session
        .relation_name_resolution_with_control(control)?;
    namespaces::current_schema_names_with_control(
        &catalog,
        &resolution,
        &resolution.current_user,
        true,
        control,
    )
}

fn object_name<'a>(
    names: &'a [String],
    control: &ProductionControl<'_>,
) -> Result<(Option<&'a str>, &'a str), SQLError> {
    match names {
        [local] => Ok((None, local)),
        [schema, local] => Ok((Some(schema), local)),
        [_, _, _] => Err(formatted(
            format_args!(
                "cross-database references are not implemented: {}",
                QualifiedNames(names)
            ),
            control,
            SQLError::Unsupported,
        )),
        _ => Err(routine(
            "42601",
            format_args!(
                "improper qualified name (too many dotted names): {}",
                QualifiedNames(names)
            ),
            control,
        )),
    }
}

fn routine(
    code: &str,
    message: std::fmt::Arguments<'_>,
    control: &ProductionControl<'_>,
) -> SQLError {
    formatted(message, control, |message| SQLError::Routine {
        sqlstate: code.into(),
        message,
    })
}

fn formatted(
    message: std::fmt::Arguments<'_>,
    control: &ProductionControl<'_>,
    build: impl FnOnce(String) -> SQLError,
) -> SQLError {
    match control.format(message) {
        Ok(message) => build(message.into_parts().0),
        Err(error) => error.into(),
    }
}

struct QualifiedNames<'a>(&'a [String]);
impl std::fmt::Display for QualifiedNames<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (index, name) in self.0.iter().enumerate() {
            if index != 0 {
                f.write_str(".")?;
            }
            f.write_str(name)?;
        }
        Ok(())
    }
}
struct EscapedName<'a>(&'a str);
impl std::fmt::Display for EscapedName<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for piece in self.0.split_inclusive('"') {
            f.write_str(piece)?;
            if piece.ends_with('"') {
                f.write_str("\"")?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
