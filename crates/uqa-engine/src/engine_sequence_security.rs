//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL role ownership and sequence privileges.

use std::collections::BTreeMap;

mod acl;

use acl::{
    grant_acl, requested_acl_privileges, revoke_acl, rewrite_acl_owner, role_has_privilege,
    select_acl_grantor, AclPrivilege, PrivilegeCheck,
};
use uqa_sql::ast::{GrantSequenceStmt, GrantSequenceTarget, SequenceRevokeBehavior};

use crate::engine_roles::{
    role_can_set, role_inherits, RoleDefinition, RoleMembership, RoleMembershipKey,
};
use crate::engine_state::SequenceSecurity;
use crate::{Engine, RelationIdentity, SQLError, Value};

struct ResolvedSequenceGrantTarget {
    requested: String,
    name: String,
    relation: RelationIdentity,
    kind: &'static str,
    require_sequence: bool,
}

pub(crate) fn role_can_view_sequence(
    security: &SequenceSecurity,
    subject: &str,
    roles: &BTreeMap<String, RoleDefinition>,
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
) -> bool {
    role_inherits(roles, memberships, subject, &security.role_owner)
        || [
            AclPrivilege::Select,
            AclPrivilege::Update,
            AclPrivilege::Usage,
        ]
        .into_iter()
        .any(|privilege| {
            role_has_privilege(
                security,
                subject,
                PrivilegeCheck {
                    privilege,
                    grant_option: false,
                },
                roles,
                memberships,
            )
        })
}

pub(crate) fn role_can_select_sequence(
    security: &SequenceSecurity,
    subject: &str,
    roles: &BTreeMap<String, RoleDefinition>,
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
) -> bool {
    role_has_privilege(
        security,
        subject,
        PrivilegeCheck {
            privilege: AclPrivilege::Select,
            grant_option: false,
        },
        roles,
        memberships,
    )
}

pub(crate) fn role_can_read_sequence_value(
    security: &SequenceSecurity,
    subject: &str,
    roles: &BTreeMap<String, RoleDefinition>,
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
) -> bool {
    [AclPrivilege::Select, AclPrivilege::Usage]
        .into_iter()
        .any(|privilege| {
            role_has_privilege(
                security,
                subject,
                PrivilegeCheck {
                    privilege,
                    grant_option: false,
                },
                roles,
                memberships,
            )
        })
}

impl Engine {
    pub(crate) fn grant_sequence_privileges(
        &self,
        statement: &GrantSequenceStmt,
    ) -> Result<(), SQLError> {
        self.prepare_explicit_transaction_writer()?;
        let targets = self.resolve_sequence_grant_targets(&statement.target)?;
        let grantees = statement
            .grantees
            .iter()
            .map(|role| self.resolve_role_reference(role))
            .collect::<Vec<_>>();
        let requested_grantor = statement
            .grantor
            .as_ref()
            .map(|role| self.resolve_role_reference(role));
        let current_user = self.current_user_name();
        let roles = self.durable.roles.read();
        validate_sequence_acl_roles(
            statement,
            &grantees,
            requested_grantor.as_deref(),
            &current_user,
            &roles,
        )?;
        validate_sequence_grant_target_kinds(&targets)?;
        let privileges = requested_acl_privileges(&statement.privileges)?;
        let memberships = self.durable.role_memberships.read();
        let mut registry = self.durable.sequence_security.write();
        let mut updates = Vec::new();
        let mut notices = Vec::new();
        for target in &targets {
            let current = registry.get(&target.relation).cloned().ok_or_else(|| {
                SQLError::Internal(format!(
                    "sequence `{}` has no security metadata",
                    target.name
                ))
            })?;
            let (next, grantable) = apply_sequence_acl(
                statement,
                &grantees,
                &privileges,
                &current_user,
                &roles,
                &memberships,
                &current,
            )?;
            if grantable != privileges.len() {
                notices.push(sequence_acl_warning(
                    statement.is_grant,
                    grantable != 0,
                    &target.relation.name,
                ));
            }
            if next != current {
                updates.push((target.name.clone(), target.relation.clone(), next));
            }
        }
        for (name, relation, security) in &updates {
            self.persist_sequence_security(name, relation, security)?;
        }
        let changed = !updates.is_empty();
        for (_, relation, security) in updates {
            registry.insert(relation, security);
        }
        drop(registry);
        drop(memberships);
        drop(roles);
        for (level, message) in notices {
            self.push_sql_notice(level, &message);
        }
        if changed {
            self.note_catalog_registry_changed();
        }
        Ok(())
    }

    fn resolve_sequence_grant_targets(
        &self,
        target: &GrantSequenceTarget,
    ) -> Result<Vec<ResolvedSequenceGrantTarget>, SQLError> {
        match target {
            GrantSequenceTarget::Sequences {
                names,
                require_sequence,
            } => {
                let mut resolved = Vec::with_capacity(names.len());
                let current_user = self.current_user_name();
                for requested in names {
                    let Some((name, kind)) =
                        self.try_resolve_sequence_relation_kind(requested, &current_user)?
                    else {
                        self.ensure_sequence_reference_schema_exists(requested)?;
                        return Err(SQLError::Routine {
                            sqlstate: "42P01".into(),
                            message: format!("relation \"{requested}\" does not exist"),
                        });
                    };
                    let relation = Self::resolved_relation_identity(&name).map_err(|error| {
                        SQLError::Internal(format!("resolve sequence `{name}`: {error}"))
                    })?;
                    resolved.push(ResolvedSequenceGrantTarget {
                        requested: requested.clone(),
                        name,
                        relation,
                        kind,
                        require_sequence: *require_sequence,
                    });
                }
                Ok(resolved)
            }
            GrantSequenceTarget::AllSequencesInSchemas { schemas } => {
                self.resolve_all_sequences_in_schemas(schemas)
            }
        }
    }

    fn resolve_all_sequences_in_schemas(
        &self,
        schemas: &[String],
    ) -> Result<Vec<ResolvedSequenceGrantTarget>, SQLError> {
        self.synchronize_catalog_registries().map_err(|error| {
            SQLError::Internal(format!("load schemas for sequence privileges: {error}"))
        })?;
        self.refresh_sequences_from_catalog().map_err(|error| {
            SQLError::Internal(format!("load sequences for privileges: {error}"))
        })?;
        let temporary_schema = self.temporary_schema_name();
        let mut resolved_schemas = Vec::with_capacity(schemas.len());
        for schema in schemas {
            let resolved = if schema == "pg_temp" {
                temporary_schema.clone()
            } else {
                schema.clone()
            };
            let exists = if resolved == temporary_schema {
                self.temporary_namespace_allocated()
            } else {
                self.has_namespace(&resolved).map_err(|error| {
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
        let sequences = self.durable.sequences.read();
        let mut targets = sequences
            .keys()
            .filter(|relation| resolved_schemas.contains(&relation.schema))
            .map(|relation| ResolvedSequenceGrantTarget {
                requested: relation.qualified_name(),
                name: relation.qualified_name(),
                relation: relation.clone(),
                kind: "sequence",
                require_sequence: true,
            })
            .collect::<Vec<_>>();
        targets.sort_by(|left, right| left.relation.cmp(&right.relation));
        Ok(targets)
    }

    fn persist_sequence_security(
        &self,
        name: &str,
        relation: &RelationIdentity,
        security: &SequenceSecurity,
    ) -> Result<(), SQLError> {
        let state = self
            .durable
            .sequences
            .read()
            .get(relation)
            .copied()
            .ok_or_else(|| SQLError::Internal(format!("sequence `{name}` disappeared")))?;
        let object_id = self
            .durable
            .sequence_object_ids
            .read()
            .get(relation)
            .copied()
            .ok_or_else(|| {
                SQLError::Internal(format!("sequence `{name}` has no object identity"))
            })?;
        let persistence = self
            .durable
            .sequence_persistence
            .read()
            .get(relation)
            .copied()
            .ok_or_else(|| {
                SQLError::Internal(format!("sequence `{name}` has no persistence metadata"))
            })?;
        if persistence == uqa_sql::ast::RelationPersistence::Temporary {
            return Ok(());
        }
        let Some(catalog) = self.storage.catalog.as_ref() else {
            return Ok(());
        };
        let row = Self::sequence_row(name, object_id, state, persistence, security)
            .map_err(|error| SQLError::Internal(format!("build sequence catalog row: {error}")))?;
        if !catalog
            .replace_sequence_row(&row)
            .map_err(|error| SQLError::Internal(format!("persist sequence privileges: {error}")))?
        {
            return Err(SQLError::Internal(format!(
                "sequence `{name}` disappeared during privilege change"
            )));
        }
        Ok(())
    }

    pub(crate) fn ensure_sequence_nextval_privilege(
        &self,
        name: &str,
        relation: &RelationIdentity,
    ) -> Result<(), SQLError> {
        self.ensure_sequence_value_privilege(
            name,
            relation,
            &[AclPrivilege::Usage, AclPrivilege::Update],
        )
    }

    pub(crate) fn ensure_sequence_currval_privilege(
        &self,
        name: &str,
        relation: &RelationIdentity,
    ) -> Result<(), SQLError> {
        self.ensure_sequence_value_privilege(
            name,
            relation,
            &[AclPrivilege::Usage, AclPrivilege::Select],
        )
    }

    pub(crate) fn ensure_sequence_setval_privilege(
        &self,
        name: &str,
        relation: &RelationIdentity,
    ) -> Result<(), SQLError> {
        self.ensure_sequence_value_privilege(name, relation, &[AclPrivilege::Update])
    }

    fn ensure_sequence_value_privilege(
        &self,
        name: &str,
        relation: &RelationIdentity,
        privileges: &[AclPrivilege],
    ) -> Result<(), SQLError> {
        let security = self
            .durable
            .sequence_security
            .read()
            .get(relation)
            .cloned()
            .ok_or_else(|| {
                SQLError::Internal(format!("sequence `{name}` has no security metadata"))
            })?;
        let current_user = self.current_user_name();
        let roles = self.durable.roles.read();
        let memberships = self.durable.role_memberships.read();
        if privileges.iter().any(|privilege| {
            role_has_privilege(
                &security,
                &current_user,
                PrivilegeCheck {
                    privilege: *privilege,
                    grant_option: false,
                },
                &roles,
                &memberships,
            )
        }) {
            return Ok(());
        }
        Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: format!("permission denied for sequence {}", relation.name),
        })
    }

    pub(crate) fn has_sequence_privilege_value(
        &self,
        arguments: &[Value],
    ) -> Result<Value, SQLError> {
        if arguments.iter().any(|argument| argument == &Value::Null) {
            return Ok(Value::Null);
        }
        let (subject_value, sequence_value, privilege_value) = match arguments {
            [sequence, privilege] => (None, sequence, privilege),
            [subject, sequence, privilege] => (Some(subject), sequence, privilege),
            _ => {
                return Err(SQLError::BadArity {
                    name: "has_sequence_privilege".into(),
                    expected: "2 or 3".into(),
                    actual: arguments.len(),
                })
            }
        };
        let current_user = subject_value.is_none().then(|| self.current_user_name());
        let subject = {
            let roles = self.durable.roles.read();
            subject_value.map_or_else(
                || Ok(current_user),
                |value| resolve_sequence_privilege_role(value, &roles),
            )?
        };
        let Some((_name, relation)) = self.resolve_sequence_privilege_target(sequence_value)?
        else {
            return Ok(Value::Null);
        };
        let privilege = match privilege_value {
            Value::Str(privilege) | Value::FixedChar(privilege) => privilege,
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "has_sequence_privilege privilege must be text, got {other:?}"
                )))
            }
        };
        let checks = acl::parse_privilege_checks(privilege)?;
        let Some(subject) = subject else {
            return Ok(Value::Bool(false));
        };
        let security = self
            .durable
            .sequence_security
            .read()
            .get(&relation)
            .cloned()
            .ok_or_else(|| {
                SQLError::Internal(format!(
                    "sequence `{}` has no security metadata",
                    relation.qualified_name()
                ))
            })?;
        let roles = self.durable.roles.read();
        let memberships = self.durable.role_memberships.read();
        Ok(Value::Bool(checks.into_iter().any(|check| {
            role_has_privilege(&security, &subject, check, &roles, &memberships)
        })))
    }

    fn resolve_sequence_privilege_target(
        &self,
        value: &Value,
    ) -> Result<Option<(String, RelationIdentity)>, SQLError> {
        match value {
            Value::Str(reference) | Value::FixedChar(reference) => {
                if let Ok(oid) = reference.parse::<i64>() {
                    return self.resolve_sequence_privilege_oid(oid);
                }
                let current_user = self.current_user_name();
                let Some((name, kind)) =
                    self.try_resolve_sequence_relation_kind(reference, &current_user)?
                else {
                    self.ensure_sequence_reference_schema_exists(reference)?;
                    return Err(SQLError::Routine {
                        sqlstate: "42P01".into(),
                        message: format!("relation \"{reference}\" does not exist"),
                    });
                };
                if kind != "sequence" {
                    return Err(SQLError::Routine {
                        sqlstate: "42809".into(),
                        message: format!("\"{reference}\" is not a sequence"),
                    });
                }
                let relation = Self::resolved_relation_identity(&name).map_err(|error| {
                    SQLError::Internal(format!("resolve sequence `{name}`: {error}"))
                })?;
                Ok(Some((name, relation)))
            }
            Value::Int(oid) => self.resolve_sequence_privilege_oid(*oid),
            other => Err(SQLError::TypeMismatch(format!(
                "has_sequence_privilege sequence must be text or oid, got {other:?}"
            ))),
        }
    }

    fn resolve_sequence_privilege_oid(
        &self,
        oid: i64,
    ) -> Result<Option<(String, RelationIdentity)>, SQLError> {
        self.refresh_sequences_from_catalog().map_err(|error| {
            SQLError::Internal(format!("load sequences for privilege inquiry: {error}"))
        })?;
        if let Some((relation, _)) = self
            .durable
            .sequence_object_ids
            .read()
            .iter()
            .find(|(_, object_id)| crate::sql::sequence_relation_oid(**object_id) == oid)
        {
            return Ok(Some((relation.qualified_name(), relation.clone())));
        }
        if let Some((name, _kind)) = crate::sql::resolve_regclass_kind_by_oid(self, oid)? {
            return Err(SQLError::Routine {
                sqlstate: "42809".into(),
                message: format!("\"{name}\" is not a sequence"),
            });
        }
        Ok(None)
    }

    pub(crate) fn ensure_sequence_owner(
        &self,
        name: &str,
        relation: &RelationIdentity,
    ) -> Result<String, SQLError> {
        let owner = self
            .durable
            .sequence_security
            .read()
            .get(relation)
            .map(|security| security.role_owner.clone())
            .ok_or_else(|| {
                SQLError::Internal(format!("sequence `{name}` has no security metadata"))
            })?;
        if self.current_user_has_role_privileges(&owner) {
            return Ok(owner);
        }
        Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: format!("must be owner of sequence {}", relation.name),
        })
    }

    pub(crate) fn alter_sequence_role_owner_inner(
        &self,
        name: &str,
        relation: &RelationIdentity,
        requested_owner: &str,
    ) -> Result<(), SQLError> {
        let current_owner = self.ensure_sequence_owner(name, relation)?;
        let new_owner = self.resolve_role_reference(requested_owner);
        let roles = self.durable.roles.read();
        if !roles.contains_key(&new_owner) {
            return Err(SQLError::Routine {
                sqlstate: "42704".into(),
                message: format!("role \"{new_owner}\" does not exist"),
            });
        }
        let memberships = self.durable.role_memberships.read();
        let current_user = self.current_user_name();
        if !role_can_set(&roles, &memberships, &current_user, &new_owner) {
            return Err(SQLError::Routine {
                sqlstate: "42501".into(),
                message: format!("must be able to SET ROLE \"{new_owner}\""),
            });
        }
        let state = self
            .durable
            .sequences
            .read()
            .get(relation)
            .copied()
            .ok_or_else(|| SQLError::Internal(format!("sequence `{name}` disappeared")))?;
        if state.owner.is_some() {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: format!("cannot change owner of sequence \"{}\"", relation.name),
            });
        }
        if current_owner == new_owner {
            return Ok(());
        }
        self.ensure_schema_privilege(
            &relation.schema,
            &new_owner,
            crate::engine_schema_security::SchemaAclPrivilege::Create,
        )?;
        let mut security = self
            .durable
            .sequence_security
            .read()
            .get(relation)
            .cloned()
            .ok_or_else(|| {
                SQLError::Internal(format!("sequence `{name}` has no security metadata"))
            })?;
        rewrite_acl_owner(&mut security, &new_owner);
        self.persist_sequence_security(name, relation, &security)?;
        self.durable
            .sequence_security
            .write()
            .insert(relation.clone(), security);
        drop(memberships);
        drop(roles);
        self.note_catalog_registry_changed();
        Ok(())
    }
}

fn apply_sequence_acl(
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

fn validate_sequence_acl_roles(
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

fn resolve_sequence_privilege_role(
    value: &Value,
    roles: &BTreeMap<String, RoleDefinition>,
) -> Result<Option<String>, SQLError> {
    match value {
        Value::Str(name) | Value::FixedChar(name) => {
            if roles.contains_key(name) {
                Ok(Some(name.clone()))
            } else {
                Err(SQLError::Routine {
                    sqlstate: "42704".into(),
                    message: format!("role \"{name}\" does not exist"),
                })
            }
        }
        Value::Int(oid) => Ok(roles
            .values()
            .find(|role| role.oid == *oid)
            .map(|role| role.name.clone())),
        other => Err(SQLError::TypeMismatch(format!(
            "has_sequence_privilege role must be name or oid, got {other:?}"
        ))),
    }
}

fn validate_sequence_grant_target_kinds(
    targets: &[ResolvedSequenceGrantTarget],
) -> Result<(), SQLError> {
    for target in targets {
        if target.kind == "sequence" {
            continue;
        }
        if !target.require_sequence {
            return Err(SQLError::Unsupported(
                "table privileges are not supported".into(),
            ));
        }
        return Err(SQLError::Routine {
            sqlstate: "42809".into(),
            message: format!("\"{}\" is not a sequence", target.requested),
        });
    }
    Ok(())
}

fn sequence_acl_warning(is_grant: bool, partial: bool, name: &str) -> (&'static str, String) {
    let message = match (is_grant, partial) {
        (true, true) => format!("not all privileges were granted for \"{name}\""),
        (true, false) => format!("no privileges were granted for \"{name}\""),
        (false, true) => format!("not all privileges could be revoked for \"{name}\""),
        (false, false) => format!("no privileges could be revoked for \"{name}\""),
    };
    ("WARNING", message)
}
