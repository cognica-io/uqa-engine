//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sequence GRANT target binding, namespace rules and ACL candidates.

use super::{
    sequence::{grant_acl, revoke_acl, select_acl_grantor, AclPrivilege},
    sequence_inquiry::SequencePrivilegeResolution,
    SequenceSecurity,
};
use crate::{
    ast::{GrantSequenceStmt, SequenceRevokeBehavior},
    catalog::{
        resolution::RelationResolution,
        roles::{RoleDefinition, RoleMembership, RoleMembershipKey},
    },
    SQLError,
};
use std::collections::BTreeMap;
use uqa_core::RelationIdentity;

pub trait SequenceGrantNamespace {
    fn temporary_schema_name(&self) -> String;
    fn temporary_namespace_allocated(&self) -> bool;
    fn has_namespace(&self, name: &str) -> Result<bool, String>;
}

pub struct ResolvedSequenceGrantTarget {
    pub requested: String,
    pub name: String,
    pub relation: RelationIdentity,
    pub kind: &'static str,
}

pub fn bind_named_sequence_grants(
    resolution: &dyn SequencePrivilegeResolution,
    names: &[String],
) -> Result<Vec<ResolvedSequenceGrantTarget>, SQLError> {
    let mut resolved = Vec::with_capacity(names.len());
    for requested in names {
        let (name, kind) = match resolution.visible_relation_kind(requested)? {
            RelationResolution::Found(name, kind) => (name, kind),
            RelationResolution::MissingSchema(schema) => {
                return Err(SQLError::Routine {
                    sqlstate: "3F000".into(),
                    message: format!("schema \"{schema}\" does not exist"),
                });
            }
            RelationResolution::MissingRelation => {
                return Err(SQLError::Routine {
                    sqlstate: "42P01".into(),
                    message: format!("relation \"{requested}\" does not exist"),
                });
            }
        };
        let relation = RelationIdentity::from_legacy_name(&name)
            .map_err(|error| SQLError::Internal(format!("resolve sequence `{name}`: {error}")))?;
        resolved.push(ResolvedSequenceGrantTarget {
            requested: requested.clone(),
            name,
            relation,
            kind,
        });
    }
    Ok(resolved)
}

pub fn bind_sequence_grant_schemas(
    namespace: &dyn SequenceGrantNamespace,
    schemas: &[String],
) -> Result<Vec<String>, SQLError> {
    let temporary_schema = namespace.temporary_schema_name();
    let mut resolved_schemas = Vec::with_capacity(schemas.len());
    for schema in schemas {
        let resolved = if schema == "pg_temp" {
            temporary_schema.clone()
        } else {
            schema.clone()
        };
        let exists = if resolved == temporary_schema {
            namespace.temporary_namespace_allocated()
        } else {
            namespace.has_namespace(&resolved).map_err(|error| {
                SQLError::Internal(format!("resolve schema `{schema}`: {error}"))
            })?
        };
        if !exists {
            return Err(SQLError::Routine {
                sqlstate: "3F000".into(),
                message: format!("schema \"{schema}\" does not exist"),
            });
        }
        if !resolved_schemas.contains(&resolved) {
            resolved_schemas.push(resolved);
        }
    }
    Ok(resolved_schemas)
}

pub fn sequence_grants_in_schemas<'a>(
    resolved_schemas: &[String],
    sequences: impl Iterator<Item = &'a RelationIdentity>,
) -> Vec<ResolvedSequenceGrantTarget> {
    let mut targets = sequences
        .filter(|relation| resolved_schemas.contains(&relation.schema))
        .map(|relation| ResolvedSequenceGrantTarget {
            requested: relation.qualified_name(),
            name: relation.qualified_name(),
            relation: relation.clone(),
            kind: "sequence",
        })
        .collect::<Vec<_>>();
    targets.sort_by(|left, right| left.relation.cmp(&right.relation));
    targets
}

pub fn apply_sequence_acl(
    statement: &GrantSequenceStmt,
    grantees: &[String],
    privileges: &[AclPrivilege],
    current_user: &str,
    roles: &BTreeMap<String, RoleDefinition>,
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
    current: &SequenceSecurity,
) -> Result<(SequenceSecurity, usize), SQLError> {
    let grantors = privileges
        .iter()
        .map(|privilege| {
            (
                *privilege,
                select_acl_grantor(current, *privilege, current_user, roles, memberships),
            )
        })
        .collect::<Vec<_>>();
    let grantable = grantors
        .iter()
        .filter(|(_, grantor)| grantor.is_some())
        .count();
    let mut next = current.clone();
    for (privilege, grantor) in grantors {
        let Some(grantor) = grantor else {
            continue;
        };
        if statement.is_grant {
            grant_acl(
                &mut next,
                privilege,
                grantees,
                &grantor,
                statement.grant_option,
            );
        } else {
            revoke_acl(
                &mut next,
                privilege,
                grantees,
                &grantor,
                statement.grant_option_only,
                statement.revoke_behavior == SequenceRevokeBehavior::Cascade,
            )?;
        }
    }
    Ok((next, grantable))
}

pub fn validate_sequence_acl_roles(
    statement: &GrantSequenceStmt,
    grantees: &[String],
    requested_grantor: Option<&str>,
    current_user: &str,
    roles: &BTreeMap<String, RoleDefinition>,
) -> Result<(), SQLError> {
    for role in grantees {
        if role != "PUBLIC" && !roles.contains_key(role) {
            return Err(SQLError::Routine {
                sqlstate: "42704".into(),
                message: format!("role \"{role}\" does not exist"),
            });
        }
    }
    if statement.is_grant && statement.grant_option && grantees.iter().any(|role| role == "PUBLIC")
    {
        return Err(SQLError::Routine {
            sqlstate: "0LP01".into(),
            message: "grant options can only be granted to roles".into(),
        });
    }
    if let Some(requested_grantor) = requested_grantor {
        if !roles.contains_key(requested_grantor) {
            return Err(SQLError::Routine {
                sqlstate: "42704".into(),
                message: format!("role \"{requested_grantor}\" does not exist"),
            });
        }
        if requested_grantor != current_user {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: "grantor must be current user".into(),
            });
        }
    }
    Ok(())
}

pub fn validate_sequence_grant_target_kinds(
    targets: &[ResolvedSequenceGrantTarget],
) -> Result<(), SQLError> {
    for target in targets {
        if target.kind == "sequence" {
            continue;
        }
        return Err(SQLError::Routine {
            sqlstate: "42809".into(),
            message: format!("\"{}\" is not a sequence", target.requested),
        });
    }
    Ok(())
}

pub fn sequence_acl_warning(is_grant: bool, partial: bool, name: &str) -> (&'static str, String) {
    let message = match (is_grant, partial) {
        (true, true) => format!("not all privileges were granted for \"{name}\""),
        (true, false) => format!("no privileges were granted for \"{name}\""),
        (false, true) => format!("not all privileges could be revoked for \"{name}\""),
        (false, false) => format!("no privileges could be revoked for \"{name}\""),
    };
    ("WARNING", message)
}
