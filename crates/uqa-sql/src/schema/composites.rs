//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The attribute checks of `DefineCompositeType` in `PostgreSQL`'s order: `MergeAttributes` rejects too many attributes and then a repeated name, `BuildDescForRelation` resolves each attribute's type, requires `USAGE` on it, derives its collation and rejects `SETOF`, and `CheckAttributeNamesTypes` then rejects pseudo-types.

use crate::ast::{ColumnType, CompositeAttributeDefinition};
use crate::catalog::composite_type::StoredCompositeAttribute;
use crate::type_resolution::{resolve_declared_column_type, FunctionTypeResolver};
use crate::SQLError;

/// `renameatt_internal` checks the source before the destination; standalone composites have no system attributes.
pub fn validate_renamed_attribute(
    definition: &crate::catalog::composite_type::StoredComposite,
    from: &str,
    to: &str,
) -> Result<(), SQLError> {
    super::columns::renamed_column_position(
        &definition.identity.name,
        definition
            .live_attributes()
            .map(|attribute| attribute.name.as_str()),
        from,
        to,
        false,
    )
    .map(|_| ())
}

/// Resolve a live composite attribute before dependency traversal. Composite relations have no system columns.
pub fn validate_removed_attribute(
    definition: &crate::catalog::composite_type::StoredComposite,
    removal: &crate::ast::CompositeAttributeRemoval,
) -> Result<bool, SQLError> {
    if definition
        .live_attributes()
        .any(|attribute| attribute.name == removal.name)
    {
        return Ok(true);
    }
    if removal.if_exists {
        Ok(false)
    } else {
        Err(super::columns::undefined_relation_column(
            &definition.identity.name,
            &removal.name,
        ))
    }
}

/// `ATExecAddColumn` for a composite relation, after its owner and kind have been checked.
pub fn prepare_added_attribute(
    types: &dyn FunctionTypeResolver,
    composites: &dyn crate::expr::composites::CompositeTypeCatalog,
    definition: &crate::catalog::composite_type::StoredComposite,
    addition: &crate::ast::CompositeAttributeAddition,
) -> Result<StoredCompositeAttribute, SQLError> {
    let attribute = &addition.attribute;
    if definition
        .live_attributes()
        .any(|current| current.name == attribute.name)
    {
        return Err(SQLError::Routine {
            sqlstate: "42701".into(),
            message: format!(
                "column \"{}\" of relation \"{}\" already exists",
                attribute.name, definition.identity.name
            ),
        });
    }
    super::table_creation::column_declarations::check_serial_array(&addition.declaration)?;
    let number = definition.next_attribute_number();
    if usize::try_from(number).map_or(true, |number| number > MAX_ATTRIBUTES) {
        return Err(SQLError::Routine {
            sqlstate: "54011".into(),
            message: format!("tables can have at most {MAX_ATTRIBUTES} columns"),
        });
    }
    let declared = match &attribute.ty {
        ColumnType::Named(name) if addition.declaration.serial => {
            crate::compiler::compile_retained_type_declaration(name)?
        }
        ColumnType::Named(name) => crate::compiler::compile_retained_type_reference(name)?,
        other => other.clone(),
    };
    let ty = resolve_declared_column_type(types, &declared)?;
    types.require_type_usage(&ty)?;
    let collation = attribute_collation(&ty, attribute.collation.as_deref())?;
    if attribute.setof {
        return Err(SQLError::Routine {
            sqlstate: "42P16".into(),
            message: format!("column \"{}\" cannot be declared SETOF", attribute.name),
        });
    }
    super::columns::validate_postgres_relation_column_type(&attribute.name, &ty)?;
    if crate::expr::composites::type_contains_composite(&ty, definition.oid, composites)? {
        return Err(SQLError::Routine {
            sqlstate: "42P16".into(),
            message: format!(
                "composite type {} cannot be made a member of itself",
                definition.identity.name
            ),
        });
    }
    Ok(StoredCompositeAttribute {
        name: attribute.name.clone(),
        ty,
        collation,
        number,
        dropped: false,
    })
}

/// `MaxHeapAttributeNumber`.
pub const MAX_ATTRIBUTES: usize = 1600;

/// Whether values of a type carry a collation.
#[must_use]
pub fn type_is_collatable(ty: &ColumnType) -> bool {
    match ty {
        ColumnType::Text
        | ColumnType::Varchar(_)
        | ColumnType::Bpchar
        | ColumnType::Character(_)
        | ColumnType::Name
        | ColumnType::PgNodeTree => true,
        ColumnType::Domain { base, .. } | ColumnType::Array(base) => type_is_collatable(base),
        _ => false,
    }
}

pub use crate::catalog::collations::builtin_collation_oid;

/// `GetColumnDefCollation`: an explicit collation must exist and the type must be collatable. The canonical collation name is kept.
fn attribute_collation(
    ty: &ColumnType,
    collation: Option<&str>,
) -> Result<Option<String>, SQLError> {
    let Some(collation) = collation else {
        return Ok(None);
    };
    let names = crate::compiler::parse_regobject_name(collation)
        .ok_or_else(|| SQLError::Internal(format!("malformed collation name {collation}")))?;
    let (schema, name) = match names.as_slice() {
        [name] => (None, name.as_str()),
        [schema, name] => (Some(schema.as_str()), name.as_str()),
        _ => {
            return Err(SQLError::Internal(format!(
                "malformed collation name {collation}"
            )))
        }
    };
    if schema.is_some_and(|schema| schema != "pg_catalog") || builtin_collation_oid(name).is_none()
    {
        return Err(SQLError::Routine {
            sqlstate: "42704".into(),
            message: format!(
                "collation \"{}\" for encoding \"UTF8\" does not exist",
                names.join(".")
            ),
        });
    }
    if !type_is_collatable(ty) {
        return Err(SQLError::Routine {
            sqlstate: "42804".into(),
            message: format!("collations are not supported by type {}", ty.display_name()),
        });
    }
    Ok(Some(name.to_owned()))
}

/// Check and bind the declared attributes of a new composite type, numbering them from one.
pub fn prepare_composite_attributes(
    types: &dyn FunctionTypeResolver,
    attributes: &[CompositeAttributeDefinition],
) -> Result<Vec<StoredCompositeAttribute>, SQLError> {
    if attributes.len() > MAX_ATTRIBUTES {
        return Err(SQLError::Routine {
            sqlstate: "54011".into(),
            message: format!("tables can have at most {MAX_ATTRIBUTES} columns"),
        });
    }
    for (position, attribute) in attributes.iter().enumerate() {
        if attributes[position + 1..]
            .iter()
            .any(|later| later.name == attribute.name)
        {
            return Err(SQLError::Routine {
                sqlstate: "42701".into(),
                message: format!("column \"{}\" specified more than once", attribute.name),
            });
        }
    }
    let mut prepared = Vec::with_capacity(attributes.len());
    for (position, attribute) in attributes.iter().enumerate() {
        let ty = resolve_declared_column_type(types, &attribute.ty)?;
        types.require_type_usage(&ty)?;
        let collation = attribute_collation(&ty, attribute.collation.as_deref())?;
        if attribute.setof {
            return Err(SQLError::Routine {
                sqlstate: "42P16".into(),
                message: format!("column \"{}\" cannot be declared SETOF", attribute.name),
            });
        }
        prepared.push(StoredCompositeAttribute {
            name: attribute.name.clone(),
            ty,
            collation,
            number: i16::try_from(position + 1)
                .map_err(|_| SQLError::Internal("composite attribute number overflow".into()))?,
            dropped: false,
        });
    }
    for attribute in &prepared {
        super::columns::validate_postgres_relation_column_type(&attribute.name, &attribute.ty)?;
    }
    Ok(prepared)
}

#[cfg(test)]
mod tests;
