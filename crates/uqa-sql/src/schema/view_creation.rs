//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! View target namespaces, replacement row types, and materialized-view declarations.
use crate::{ast::RelationPersistence, RowSchema, SQLError};
use uqa_core::RelationIdentity;

pub trait ViewCreationNamespace {
    fn temporary_schema_name(&self) -> String;
    fn temporary_target(&self, name: &str) -> Result<String, SQLError>;
    fn persistent_target(&self, name: &str) -> Result<String, SQLError>;
}

pub fn view_creation_target(
    namespace: &dyn ViewCreationNamespace,
    name: &str,
    persistence: RelationPersistence,
    uses_temporary_relation: bool,
) -> Result<(String, RelationPersistence), SQLError> {
    let persistence = if uses_temporary_relation {
        RelationPersistence::Temporary
    } else {
        persistence
    };
    let name = if persistence == RelationPersistence::Temporary {
        let (schema, _) = RelationIdentity::parse_reference(name).map_err(SQLError::Unsupported)?;
        if uses_temporary_relation
            && schema.as_deref().is_some_and(|schema| {
                schema != "pg_temp" && schema != namespace.temporary_schema_name()
            })
        {
            namespace.persistent_target(name)?;
        }
        namespace.temporary_target(name)?
    } else {
        namespace.persistent_target(name)?
    };
    Ok((name, persistence))
}

pub fn replacement_is_view(
    name: &str,
    kind: Option<&str>,
    or_replace: bool,
) -> Result<bool, SQLError> {
    match kind {
        Some(_) if !or_replace => Err(SQLError::Routine {
            sqlstate: "42P07".into(),
            message: format!("relation \"{name}\" already exists"),
        }),
        Some("view") => Ok(true),
        Some(kind) => Err(SQLError::Routine {
            sqlstate: "42809".into(),
            message: format!("\"{name}\" is not a view; it is a {kind}"),
        }),
        None => Ok(false),
    }
}

pub fn validate_replacement_schema(old: &RowSchema, new: &RowSchema) -> Result<(), SQLError> {
    if new.len() < old.len() {
        return Err(SQLError::Routine {
            sqlstate: "42P16".into(),
            message: "cannot drop columns from view".into(),
        });
    }
    for position in 0..old.len() {
        let old_name = old
            .public_name(position)
            .unwrap_or(&old.columns()[position]);
        let new_name = new
            .public_name(position)
            .unwrap_or(&new.columns()[position]);
        if old_name != new_name {
            return Err(SQLError::Routine {
                sqlstate: "42P16".into(),
                message: format!(
                    "cannot change name of view column \"{old_name}\" to \"{new_name}\""
                ),
            });
        }
        if old.column_type(position) != new.column_type(position) {
            return Err(SQLError::Routine {
                sqlstate: "42P16".into(),
                message: format!("cannot change data type of view column \"{old_name}\""),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
