//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! View target namespaces, replacement row types, and materialized-view declarations.
use crate::{ast::RelationPersistence, RowSchema, SQLError};
use uqa_core::RelationIdentity;

pub trait ViewCreationNamespace {
    /// `RangeVarGetAndCheckCreationNamespace` for the view: its canonical name and the persistence it takes there.
    fn relation_target(
        &self,
        name: &str,
        persistence: RelationPersistence,
    ) -> Result<(String, RelationPersistence), SQLError>;
}

/// Whether `DefineView` makes a view temporary because its query uses a temporary relation, which it reports with a notice before it resolves the view's namespace.
pub fn view_becomes_temporary(
    persistence: RelationPersistence,
    uses_temporary_relation: bool,
) -> bool {
    uses_temporary_relation && persistence == RelationPersistence::Permanent
}

/// The notice `DefineView` reports for a view that its query makes temporary, naming the view as written without its schema.
pub fn temporary_view_notice(name: &str) -> Result<crate::SQLNotice, SQLError> {
    let (_, relation) = RelationIdentity::parse_reference(name).map_err(SQLError::Unsupported)?;
    Ok(crate::SQLNotice::notice(format!(
        "view \"{relation}\" will be a temporary view"
    )))
}

/// The view's canonical name and persistence: a view whose query uses a temporary relation is temporary, and then goes where a temporary relation of its name goes.
pub fn view_creation_target(
    namespace: &dyn ViewCreationNamespace,
    name: &str,
    persistence: RelationPersistence,
    uses_temporary_relation: bool,
) -> Result<(String, RelationPersistence), SQLError> {
    let persistence = if view_becomes_temporary(persistence, uses_temporary_relation) {
        RelationPersistence::Temporary
    } else {
        persistence
    };
    namespace.relation_target(name, persistence)
}

/// `DefineVirtualRelation`: a relation that already has the name is a collision, or with `OR REPLACE` must be a view. Both diagnostics name the relation without its schema.
pub fn replacement_is_view(
    name: &str,
    kind: Option<&str>,
    or_replace: bool,
) -> Result<bool, SQLError> {
    let local = || {
        uqa_core::RelationIdentity::from_legacy_name(name)
            .map_or_else(|_| name.to_string(), |relation| relation.name)
    };
    match kind {
        Some(_) if !or_replace => Err(SQLError::Routine {
            sqlstate: "42P07".into(),
            message: format!("relation \"{}\" already exists", local()),
        }),
        Some("view") => Ok(true),
        Some(_) => Err(SQLError::Routine {
            sqlstate: "42809".into(),
            message: format!("\"{}\" is not a view", local()),
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
