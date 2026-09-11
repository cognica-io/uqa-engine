//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind GRANT relation identities without collapsing missing-schema diagnostics.
use super::ResolvedTableGrantTarget;
use crate::{catalog::resolution::RelationResolution, SQLError};
use uqa_core::RelationIdentity;
pub trait TableGrantResolution {
    fn resolve_visible_relation_kind(&self, name: &str) -> Result<RelationResolution, SQLError>;
}
pub fn bind_named_table_grants(
    resolution: &dyn TableGrantResolution,
    names: &[String],
) -> Result<Vec<ResolvedTableGrantTarget>, SQLError> {
    let mut resolved = Vec::with_capacity(names.len());
    for requested in names {
        let (name, kind) = match resolution.resolve_visible_relation_kind(requested)? {
            RelationResolution::Found(name, kind) => (name, kind),
            RelationResolution::MissingSchema(schema) => {
                return Err(SQLError::Routine {
                    sqlstate: "3F000".into(),
                    message: format!("schema \"{schema}\" does not exist"),
                })
            }
            RelationResolution::MissingRelation => {
                return Err(SQLError::Routine {
                    sqlstate: "42P01".into(),
                    message: format!("relation \"{requested}\" does not exist"),
                })
            }
        };
        let relation = RelationIdentity::from_legacy_name(&name)
            .map_err(|error| SQLError::Internal(format!("resolve table `{name}`: {error}")))?;
        resolved.push(ResolvedTableGrantTarget {
            requested: requested.clone(),
            name,
            relation,
            kind,
        });
    }
    Ok(resolved)
}
