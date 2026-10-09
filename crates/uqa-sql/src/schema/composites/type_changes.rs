//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Composite attribute type preparation and dependency checks, before any catalog write.

use crate::ast::{ColumnType, CompositeAttributeDefinition};
use crate::catalog::composite_type::{StoredComposite, StoredCompositeAttribute};
use crate::catalog::dependencies::{
    DependencyGraph, ObjectAddress, ATTRIBUTE_DEFAULT_CLASS, CONSTRAINT_CLASS, PROCEDURE_CLASS,
    RELATION_CLASS, REWRITE_CLASS, TRIGGER_CLASS, TYPE_CLASS,
};
use crate::expr::composites::CompositeTypeCatalog;
use crate::type_resolution::FunctionTypeResolver;
use crate::SQLError;

/// `ATPrepAlterColumnType` binds each original live attribute before resolving its new type. Unlike ADD ATTRIBUTE, this path ignores the grammar's SETOF marker.
pub fn prepare_attribute_type(
    types: &dyn FunctionTypeResolver,
    composites: &dyn CompositeTypeCatalog,
    definition: &StoredComposite,
    change: &CompositeAttributeDefinition,
) -> Result<StoredCompositeAttribute, SQLError> {
    let original = definition
        .live_attributes()
        .find(|attribute| attribute.name == change.name)
        .ok_or_else(|| {
            super::super::columns::undefined_relation_column(
                &definition.identity.name,
                &change.name,
            )
        })?;
    let declared = match &change.ty {
        ColumnType::Named(name) => crate::compiler::compile_retained_type_reference(name)?,
        ty => ty.clone(),
    };
    super::prepare_attribute(
        types,
        composites,
        definition,
        change,
        &declared,
        original.number,
        false,
    )
}

/// Whether a relation dependency reaches stored values or another container's row type. A transient expression reference returns `None`.
pub enum CompositeStorageUse<'a> {
    Stored { relation: &'a str, column: &'a str },
    RowType(u32),
}

/// `find_composite_type_dependencies` follows arrays, domains and relation row types in dependency order, including empty physical relations. It never reads row payloads.
pub fn reject_stored_uses<'a>(
    graph: &DependencyGraph,
    original: u32,
    name: &str,
    relation_use: impl Fn(ObjectAddress, u32) -> Option<CompositeStorageUse<'a>>,
) -> Result<(), SQLError> {
    let mut pending = vec![(ObjectAddress::whole(TYPE_CLASS, original), original)];
    let mut visited = std::collections::BTreeSet::new();
    while let Some((address, target)) = pending.pop() {
        if address.class_id == TYPE_CLASS {
            if !visited.insert(address.object_id) {
                continue;
            }
            let dependents = graph
                .dependents_of(address)
                .map(|edge| (edge.dependent, address.object_id))
                .collect::<Vec<_>>();
            pending.extend(dependents.into_iter().rev());
        } else if address.class_id == RELATION_CLASS {
            match relation_use(address, target) {
                Some(CompositeStorageUse::Stored { relation, column }) => return Err(SQLError::Routine {
                    sqlstate: "0A000".into(),
                    message: format!("cannot alter type \"{name}\" because column \"{relation}.{column}\" uses it"),
                }),
                Some(CompositeStorageUse::RowType(oid)) => pending.push((ObjectAddress::whole(TYPE_CLASS, oid), oid)),
                None => {}
            }
        }
    }
    Ok(())
}

/// Stored field consumers that `RememberAllDependentForRebuilding` refuses to reparse when a type changes, even if its new declaration is identical.
pub enum FieldDependent {
    ColumnDefault(String),
    DomainConstraint,
    Other,
}

pub fn reject_field_dependents(
    graph: &DependencyGraph,
    attribute: ObjectAddress,
    name: &str,
    describe: impl Fn(ObjectAddress) -> Result<Option<String>, SQLError>,
    object: impl Fn(ObjectAddress) -> FieldDependent,
) -> Result<(), SQLError> {
    for dependency in graph.dependents_of(attribute) {
        if dependency.dependent.class_id == ATTRIBUTE_DEFAULT_CLASS {
            let FieldDependent::ColumnDefault(column) = object(dependency.dependent) else {
                return Err(SQLError::Internal(
                    "attribute default dependency disappeared".into(),
                ));
            };
            return Err(SQLError::Diagnostic {
                sqlstate: "0A000".into(),
                message: "cannot alter type of a column used by a generated column".into(),
                detail: Some(format!(
                    "Column \"{name}\" is used by generated column \"{column}\"."
                )),
                hint: None,
            });
        }
        if dependency.dependent.class_id == CONSTRAINT_CLASS
            && matches!(
                object(dependency.dependent),
                FieldDependent::DomainConstraint
            )
        {
            return Err(SQLError::Routine {
                sqlstate: "XX000".into(),
                message: format!(
                    "could not identify relation associated with constraint {}",
                    dependency.dependent.object_id
                ),
            });
        }
        let usage = match dependency.dependent.class_id {
            PROCEDURE_CLASS => "used by a function or procedure",
            REWRITE_CLASS => "used by a view or rule",
            TRIGGER_CLASS => "used in a trigger definition",
            _ => continue,
        };
        let description = describe(dependency.dependent)?
            .ok_or_else(|| SQLError::Internal("attribute dependency disappeared".into()))?;
        return Err(SQLError::Diagnostic {
            sqlstate: "0A000".into(),
            message: format!("cannot alter type of a column {usage}"),
            detail: Some(format!("{description} depends on column \"{name}\"")),
            hint: None,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests;
