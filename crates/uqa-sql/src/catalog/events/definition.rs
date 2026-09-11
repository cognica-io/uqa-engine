//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Trigger and rewrite-rule declarations, relation identity checks, and stored expression binding.

mod context;
mod rules;
mod triggers;
use crate::{
    catalog::resolution::{RelationLookupMode, RelationResolution},
    SQLError,
};
pub use context::{EventAnalysisContext, EventForeignPrivileges, EventRelationCatalog};
use uqa_core::RelationIdentity;

impl EventAnalysisContext<'_> {
    pub fn event_relation_from_resolution(
        requested: &str,
        resolution: RelationResolution,
    ) -> Result<(RelationIdentity, &'static str), SQLError> {
        let (canonical, kind) = match resolution {
            RelationResolution::Found(canonical, kind)
                if matches!(kind, "table" | "view" | "materialized view") =>
            {
                (canonical, kind)
            }
            RelationResolution::Found(_, _) | RelationResolution::MissingRelation => {
                return Err(SQLError::UnknownTable(requested.to_string()));
            }
            RelationResolution::MissingSchema(schema) => {
                return Err(SQLError::Routine {
                    sqlstate: "3F000".into(),
                    message: format!("schema \"{schema}\" does not exist"),
                });
            }
        };
        let relation = RelationIdentity::from_legacy_name(&canonical).map_err(|error| {
            SQLError::Internal(format!(
                "decode resolved event relation `{canonical}`: {error}"
            ))
        })?;
        Ok((relation, kind))
    }

    pub fn resolve_event_relation_kind(
        &self,
        name: &str,
        lookup_mode: RelationLookupMode,
    ) -> Result<(RelationIdentity, &'static str), SQLError> {
        let resolution = match lookup_mode {
            RelationLookupMode::Dynamic => self.relations.resolve_visible_relation_kind(name)?,
            RelationLookupMode::Bound => self.relations.resolve_bound_relation_kind(name)?,
        };
        Self::event_relation_from_resolution(name, resolution)
    }
}

impl EventAnalysisContext<'_> {
    pub fn ensure_event_relation_owner(
        &self,
        relation: &RelationIdentity,
        error_kind: Option<&str>,
    ) -> Result<(), SQLError> {
        let (owner, relation_kind) = self.catalog.event_relation_owner(relation)?;
        if self.authority.current_user_has_role_privileges(&owner) {
            return Ok(());
        }
        Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: format!(
                "must be owner of {} {}",
                error_kind.unwrap_or(relation_kind),
                relation.name
            ),
        })
    }
}

#[cfg(test)]
mod tests;

pub fn duplicate_object(kind: &str, name: &str, table: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "42710".into(),
        message: format!("{kind} \"{name}\" for relation \"{table}\" already exists"),
    }
}

pub fn undefined_object(kind: &str, name: &str, table: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "42704".into(),
        message: format!("{kind} \"{name}\" for table \"{table}\" does not exist"),
    }
}

pub fn undefined_rule(name: &str, relation: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "42704".into(),
        message: format!("rule \"{name}\" for relation \"{relation}\" does not exist"),
    }
}

pub mod lookup;
