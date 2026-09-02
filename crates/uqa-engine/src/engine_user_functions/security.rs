//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine ownership, execution privileges, and security attributes.

use std::collections::{BTreeMap, BTreeSet};

use uqa_sql::ast::{
    AlterRoutineOwnerStmt, AlterRoutineStmt, CreateFunction, GrantRoutineStmt, RoutineAclEntry,
    RoutineConfigAction, RoutineRevokeBehavior,
};
use uqa_sql::SQLError;

use crate::{
    engine_roles::{
        role_can_set, role_inherits, RoleDefinition, RoleMembership, RoleMembershipKey,
    },
    Arc, Engine,
};

use super::declaration::resolve_alter_routine_identity_types;
use super::resolution::{routine_kind, routine_local_name};
use super::{builtin_routine_support_oid, SQLUserFunction};

impl Engine {
    pub(crate) fn alter_sql_routine_owner(
        &self,
        stmt: &AlterRoutineOwnerStmt,
    ) -> Result<(), SQLError> {
        let identity = AlterRoutineStmt {
            kind: stmt.kind,
            name: stmt.name.clone(),
            arg_types: stmt.arg_types.clone(),
            arg_type_references: stmt.arg_type_references.clone(),
            volatility: None,
            strict: None,
            security_definer: None,
            leakproof: None,
            parallel: None,
            support: None,
            config_actions: Vec::new(),
        };
        let requested_types = resolve_alter_routine_identity_types(self, &identity)?;
        let new_owner = self.resolve_role_reference(&stmt.new_owner);
        let current_user = self.current_user_name();
        self.prepare_explicit_transaction_writer()?;
        let roles = self.durable.roles.read();
        if !roles.contains_key(&new_owner) {
            return Err(SQLError::Routine {
                sqlstate: "42704".into(),
                message: format!("role \"{new_owner}\" does not exist"),
            });
        }
        let memberships = self.durable.role_memberships.read();
        let mut registry = self.durable.sql_user_functions.write();
        let (name, position) = self.resolve_sql_routine_alter_target(
            &registry,
            &stmt.name,
            requested_types.as_deref(),
            stmt.kind,
        )?;
        let existing = registry[&name][position].clone();
        Self::ensure_routine_owner_as(
            &existing.def,
            role_inherits(&roles, &memberships, &current_user, &existing.def.owner),
        )?;
        if !role_can_set(&roles, &memberships, &current_user, &new_owner) {
            return Err(SQLError::Routine {
                sqlstate: "42501".into(),
                message: format!("must be able to SET ROLE \"{new_owner}\""),
            });
        }
        let mut def = existing.def.clone();
        rewrite_routine_acl_owner(&mut def, &existing.def.owner, &new_owner);
        def.owner = new_owner;
        let mut next = registry.clone();
        next.get_mut(&name).expect("resolved routine key")[position] = Arc::new(SQLUserFunction {
            def,
            compiled: existing.compiled.clone(),
        });
        self.persist_sql_functions_snapshot(&next)?;
        *registry = next;
        drop(registry);
        drop(memberships);
        drop(roles);
        self.note_catalog_registry_changed();
        Ok(())
    }

    pub(crate) fn grant_sql_routine(&self, stmt: &GrantRoutineStmt) -> Result<(), SQLError> {
        let grantees = stmt
            .grantees
            .iter()
            .map(|role| self.resolve_role_reference(role))
            .collect::<Vec<_>>();
        let requested_grantor = stmt
            .grantor
            .as_ref()
            .map(|role| self.resolve_role_reference(role));
        let current_user = self.current_user_name();
        self.prepare_explicit_transaction_writer()?;
        let roles = self.durable.roles.read();
        validate_routine_acl_roles(
            stmt,
            &grantees,
            requested_grantor.as_deref(),
            &current_user,
            &roles,
        )?;
        let memberships = self.durable.role_memberships.read();
        let mut registry = self.durable.sql_user_functions.write();
        let mut resolved = Vec::with_capacity(stmt.items.len());
        for item in &stmt.items {
            let (name, position) = self.resolve_sql_routine_alter_target(
                &registry,
                &item.name,
                item.arg_types.as_deref(),
                stmt.kind,
            )?;
            let function = registry[&name][position].clone();
            let grantor =
                select_routine_acl_grantor(&function.def, &current_user, &roles, &memberships);
            resolved.push((name, position, grantor));
        }
        let mut next = registry.clone();
        let mut notices = Vec::new();
        for (name, position, grantor) in resolved {
            let existing = next[&name][position].clone();
            let Some(grantor) = grantor else {
                notices.push(routine_acl_warning(stmt.is_grant, &existing.def.name));
                continue;
            };
            let mut def = existing.def.clone();
            let changed = if stmt.is_grant {
                for grantee in &grantees {
                    grant_routine_acl(&mut def, grantee, &grantor, stmt.grant_option);
                }
                def.execute_acl != existing.def.execute_acl
            } else {
                let mut changed = false;
                for grantee in &grantees {
                    changed |= revoke_routine_acl(
                        &mut def,
                        grantee,
                        &grantor,
                        stmt.grant_option_only,
                        stmt.revoke_behavior == RoutineRevokeBehavior::Cascade,
                    )?;
                }
                if !changed {
                    notices.push(routine_acl_warning(false, &existing.def.name));
                }
                changed
            };
            if changed {
                next.get_mut(&name).expect("resolved routine key")[position] =
                    Arc::new(SQLUserFunction {
                        def,
                        compiled: existing.compiled.clone(),
                    });
            }
        }
        self.persist_sql_functions_snapshot(&next)?;
        *registry = next;
        drop(registry);
        drop(memberships);
        drop(roles);
        for (level, message) in notices {
            self.push_sql_notice(level, &message);
        }
        self.note_catalog_registry_changed();
        Ok(())
    }

    pub(crate) fn ensure_routine_execute_privilege(
        &self,
        definition: &CreateFunction,
    ) -> Result<(), SQLError> {
        let current = self.current_user_name();
        let allowed = self.current_user_is_superuser()
            || self.current_user_has_role_privileges(&definition.owner)
            || definition.execute_acl.as_ref().is_none_or(|acl| {
                acl.iter().any(|entry| {
                    entry.role == "PUBLIC"
                        || entry.role == current
                        || self.current_user_has_role_privileges(&entry.role)
                })
            });
        if allowed {
            Ok(())
        } else {
            Err(SQLError::Routine {
                sqlstate: "42501".into(),
                message: format!(
                    "permission denied for {} {}",
                    routine_kind(definition),
                    routine_local_name(&definition.name)?
                ),
            })
        }
    }

    pub(super) fn ensure_routine_owner_as(
        definition: &CreateFunction,
        current_user_has_owner_privileges: bool,
    ) -> Result<(), SQLError> {
        if current_user_has_owner_privileges {
            Ok(())
        } else {
            Err(SQLError::Routine {
                sqlstate: "42501".into(),
                message: format!(
                    "must be owner of {} {}",
                    routine_kind(definition),
                    definition.name
                ),
            })
        }
    }

    pub(super) fn validate_routine_support(&self, support: &str) -> Result<(), SQLError> {
        if builtin_routine_support_oid(support).is_none() {
            return Err(SQLError::Routine {
                sqlstate: "42883".into(),
                message: format!("function {support}(internal) does not exist"),
            });
        }
        if !self.current_user_is_superuser() {
            return Err(SQLError::Routine {
                sqlstate: "42501".into(),
                message: "must be superuser to specify a support function".into(),
            });
        }
        Ok(())
    }

    pub(super) fn apply_routine_config_actions(
        &self,
        definition: &mut CreateFunction,
    ) -> Result<(), SQLError> {
        if definition.config_actions.is_empty() {
            return Ok(());
        }
        let _guard = self.routine_config_state_guard();
        let mut result = Ok(());
        for action in std::mem::take(&mut definition.config_actions) {
            let applied = match action {
                RoutineConfigAction::Set { name, value: _ }
                | RoutineConfigAction::FromCurrent { name }
                    if name.eq_ignore_ascii_case("transaction_isolation")
                        || name.eq_ignore_ascii_case("transaction_read_only")
                        || name.eq_ignore_ascii_case("transaction_deferrable") =>
                {
                    Err(SQLError::Routine {
                        sqlstate: "0A000".into(),
                        message: format!("parameter \"{name}\" cannot be set locally in functions"),
                    })
                }
                RoutineConfigAction::Set { name, value } => self
                    .set_variable(&name, &value)
                    .and_then(|()| self.show_variable(&name))
                    .map(|value| Some((name, value))),
                RoutineConfigAction::FromCurrent { name } => {
                    self.show_variable(&name).map(|value| Some((name, value)))
                }
                RoutineConfigAction::Reset { name } => {
                    definition
                        .config
                        .retain(|(existing, _)| !existing.eq_ignore_ascii_case(&name));
                    Ok(None)
                }
                RoutineConfigAction::ResetAll => {
                    definition.config.clear();
                    Ok(None)
                }
            };
            match applied {
                Ok(Some((name, value))) => {
                    if let Some((_, existing)) = definition
                        .config
                        .iter_mut()
                        .find(|(existing, _)| existing.eq_ignore_ascii_case(&name))
                    {
                        *existing = value;
                    } else {
                        definition.config.push((name, value));
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    result = Err(error);
                    break;
                }
            }
        }
        result
    }
}

fn validate_routine_acl_roles(
    stmt: &GrantRoutineStmt,
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
    if stmt.is_grant && stmt.grant_option && grantees.iter().any(|role| role == "PUBLIC") {
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

fn routine_acl_grantor<'a>(entry: &'a RoutineAclEntry, owner: &'a str) -> &'a str {
    entry.grantor.as_deref().unwrap_or(owner)
}

fn materialize_routine_acl(definition: &mut CreateFunction) -> &mut Vec<RoutineAclEntry> {
    if definition.execute_acl.is_none() {
        definition.execute_acl = Some(vec![RoutineAclEntry {
            role: "PUBLIC".into(),
            grantor: Some(definition.owner.clone()),
            grant_option: false,
        }]);
    }
    definition
        .execute_acl
        .as_mut()
        .expect("routine ACL was materialized")
}

fn routine_grant_option_roles(definition: &CreateFunction) -> BTreeSet<String> {
    routine_grant_option_roles_for(&definition.owner, definition.execute_acl.as_deref())
}

fn routine_grant_option_roles_for(
    owner: &str,
    acl: Option<&[RoutineAclEntry]>,
) -> BTreeSet<String> {
    let mut reachable = BTreeSet::from([owner.to_string()]);
    let Some(acl) = acl else {
        return reachable;
    };
    loop {
        let mut changed = false;
        for entry in acl {
            if entry.role != "PUBLIC"
                && entry.grant_option
                && reachable.contains(routine_acl_grantor(entry, owner))
            {
                changed |= reachable.insert(entry.role.clone());
            }
        }
        if !changed {
            return reachable;
        }
    }
}

fn select_routine_acl_grantor(
    definition: &CreateFunction,
    current_user: &str,
    roles: &BTreeMap<String, RoleDefinition>,
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
) -> Option<String> {
    if role_inherits(roles, memberships, current_user, &definition.owner) {
        return Some(definition.owner.clone());
    }
    let grant_options = routine_grant_option_roles(definition);
    if grant_options.contains(current_user) {
        return Some(current_user.to_string());
    }
    definition.execute_acl.as_ref().and_then(|acl| {
        acl.iter()
            .filter(|entry| entry.role != "PUBLIC" && grant_options.contains(&entry.role))
            .find(|entry| role_inherits(roles, memberships, current_user, &entry.role))
            .map(|entry| entry.role.clone())
    })
}

fn grant_routine_acl(
    definition: &mut CreateFunction,
    grantee: &str,
    grantor: &str,
    grant_option: bool,
) {
    if grantee == definition.owner {
        return;
    }
    if definition.execute_acl.is_none()
        && grantee == "PUBLIC"
        && grantor == definition.owner
        && !grant_option
    {
        return;
    }
    let owner = definition.owner.clone();
    let acl = materialize_routine_acl(definition);
    if let Some(entry) = acl
        .iter_mut()
        .find(|entry| entry.role == grantee && routine_acl_grantor(entry, &owner) == grantor)
    {
        entry.grant_option |= grant_option;
    } else {
        acl.push(RoutineAclEntry {
            role: grantee.to_string(),
            grantor: Some(grantor.to_string()),
            grant_option,
        });
    }
}

fn revoke_routine_acl(
    definition: &mut CreateFunction,
    grantee: &str,
    grantor: &str,
    grant_option_only: bool,
    cascade: bool,
) -> Result<bool, SQLError> {
    if grantee == definition.owner {
        return Ok(false);
    }
    let owner = definition.owner.clone();
    let before_grant_options = routine_grant_option_roles(definition);
    let acl = materialize_routine_acl(definition);
    let Some(position) = acl
        .iter()
        .position(|entry| entry.role == grantee && routine_acl_grantor(entry, &owner) == grantor)
    else {
        return Ok(false);
    };
    if grant_option_only {
        if !acl[position].grant_option {
            return Ok(false);
        }
        acl[position].grant_option = false;
    } else {
        acl.remove(position);
    }
    revoke_dependent_routine_acl(definition, &before_grant_options, cascade)?;
    Ok(true)
}

fn revoke_dependent_routine_acl(
    definition: &mut CreateFunction,
    before_grant_options: &BTreeSet<String>,
    cascade: bool,
) -> Result<(), SQLError> {
    loop {
        let current_grant_options = routine_grant_option_roles(definition);
        let lost = before_grant_options
            .difference(&current_grant_options)
            .cloned()
            .collect::<BTreeSet<_>>();
        if lost.is_empty() {
            return Ok(());
        }
        let owner = definition.owner.clone();
        let dependent_exists = definition.execute_acl.as_ref().is_some_and(|acl| {
            acl.iter()
                .any(|entry| lost.contains(routine_acl_grantor(entry, &owner)))
        });
        if !dependent_exists {
            return Ok(());
        }
        if !cascade {
            return Err(SQLError::Routine {
                sqlstate: "2BP01".into(),
                message: "dependent privileges exist".into(),
            });
        }
        definition
            .execute_acl
            .as_mut()
            .expect("dependent ACLs require an explicit ACL")
            .retain(|entry| !lost.contains(routine_acl_grantor(entry, &owner)));
    }
}

fn rewrite_routine_acl_owner(definition: &mut CreateFunction, old_owner: &str, new_owner: &str) {
    let Some(acl) = definition.execute_acl.as_mut() else {
        return;
    };
    for entry in acl.iter_mut() {
        if entry.role == old_owner {
            entry.role = new_owner.to_string();
        }
        if entry.grantor.as_deref() == Some(old_owner) {
            entry.grantor = Some(new_owner.to_string());
        }
    }
    let mut merged: Vec<RoutineAclEntry> = Vec::with_capacity(acl.len());
    for entry in std::mem::take(acl) {
        if let Some(existing) = merged.iter_mut().find(|existing| {
            existing.role == entry.role
                && routine_acl_grantor(existing, new_owner)
                    == routine_acl_grantor(&entry, new_owner)
        }) {
            existing.grant_option |= entry.grant_option;
        } else {
            merged.push(entry);
        }
    }
    *acl = merged;
}

fn routine_acl_warning(is_grant: bool, name: &str) -> (&'static str, String) {
    let local_name = name.rsplit('.').next().unwrap_or(name);
    (
        "WARNING",
        if is_grant {
            format!("no privileges were granted for \"{local_name}\"")
        } else {
            format!("no privileges could be revoked for \"{local_name}\"")
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant(grantee: &str, grantor: &str) -> RoutineAclEntry {
        RoutineAclEntry {
            role: grantee.into(),
            grantor: Some(grantor.into()),
            grant_option: true,
        }
    }

    #[test]
    fn routine_grant_option_reachability_requires_an_owner_root() {
        let disconnected_cycle = [grant("delegate", "leaf"), grant("leaf", "delegate")];
        assert_eq!(
            routine_grant_option_roles_for("owner", Some(&disconnected_cycle)),
            BTreeSet::from(["owner".into()])
        );

        let rooted_cycle = [
            grant("delegate", "owner"),
            grant("leaf", "delegate"),
            grant("delegate", "leaf"),
        ];
        assert_eq!(
            routine_grant_option_roles_for("owner", Some(&rooted_cycle)),
            BTreeSet::from(["delegate".into(), "leaf".into(), "owner".into()])
        );
    }

    #[test]
    fn routine_grant_option_reachability_accepts_an_independent_owner_path() {
        let acl = [
            grant("delegate", "owner"),
            grant("leaf", "delegate"),
            grant("leaf", "owner"),
            grant("tail", "leaf"),
        ];
        assert_eq!(
            routine_grant_option_roles_for("owner", Some(&acl[2..])),
            BTreeSet::from(["leaf".into(), "owner".into(), "tail".into()])
        );
    }
}
