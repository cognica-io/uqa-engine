//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine ownership, execution privileges, and security attributes.

use uqa_sql::ast::{
    AlterRoutineOwnerStmt, AlterRoutineStmt, CreateFunction, GrantRoutineStmt, RoleAttribute,
    RoutineAclEntry, RoutineConfigAction,
};
use uqa_sql::SQLError;

use crate::{Arc, Engine};

use super::declaration::resolve_alter_routine_identity_types;
use super::lifecycle::routine_signature_label;
use super::resolution::{routine_kind, routine_signature_types};
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
        let roles = self.durable.roles.read();
        if !roles.contains_key(&new_owner) {
            return Err(SQLError::Routine {
                sqlstate: "42704".into(),
                message: format!("role \"{new_owner}\" does not exist"),
            });
        }
        let current_user_is_superuser = roles
            .get(&current_user)
            .is_some_and(|role| role.has(RoleAttribute::Superuser));
        let mut registry = self.durable.sql_user_functions.write();
        let (name, position) = self.resolve_sql_routine_alter_target(
            &registry,
            &stmt.name,
            requested_types.as_deref(),
            stmt.kind,
        )?;
        let existing = registry[&name][position].clone();
        Self::ensure_routine_owner_as(&existing.def, &current_user, current_user_is_superuser)?;
        let mut def = existing.def.clone();
        def.owner = new_owner;
        let mut next = registry.clone();
        next.get_mut(&name).expect("resolved routine key")[position] = Arc::new(SQLUserFunction {
            def,
            compiled: existing.compiled.clone(),
        });
        self.persist_sql_functions_snapshot(&next)?;
        *registry = next;
        drop(registry);
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
        let current_user = self.current_user_name();
        let roles = self.durable.roles.read();
        for role in &grantees {
            if role != "PUBLIC" && !roles.contains_key(role) {
                return Err(SQLError::Routine {
                    sqlstate: "42704".into(),
                    message: format!("role \"{role}\" does not exist"),
                });
            }
        }
        let current_user_is_superuser = roles
            .get(&current_user)
            .is_some_and(|role| role.has(RoleAttribute::Superuser));
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
            Self::ensure_routine_owner_as(&function.def, &current_user, current_user_is_superuser)?;
            resolved.push((name, position));
        }
        let mut next = registry.clone();
        for (name, position) in resolved {
            let existing = next[&name][position].clone();
            let mut def = existing.def.clone();
            let acl = def.execute_acl.get_or_insert_with(|| {
                vec![RoutineAclEntry {
                    role: "PUBLIC".into(),
                    grant_option: false,
                }]
            });
            for grantee in &grantees {
                if stmt.is_grant {
                    match acl.iter_mut().find(|entry| entry.role == *grantee) {
                        Some(entry) => entry.grant_option |= stmt.grant_option,
                        None => acl.push(RoutineAclEntry {
                            role: grantee.clone(),
                            grant_option: stmt.grant_option,
                        }),
                    }
                } else if stmt.grant_option_only {
                    if let Some(entry) = acl.iter_mut().find(|entry| entry.role == *grantee) {
                        entry.grant_option = false;
                    }
                } else {
                    acl.retain(|entry| entry.role != *grantee);
                }
            }
            acl.sort_by(|left, right| left.role.cmp(&right.role));
            next.get_mut(&name).expect("resolved routine key")[position] =
                Arc::new(SQLUserFunction {
                    def,
                    compiled: existing.compiled.clone(),
                });
        }
        self.persist_sql_functions_snapshot(&next)?;
        *registry = next;
        drop(registry);
        drop(roles);
        self.note_catalog_registry_changed();
        Ok(())
    }

    pub(crate) fn ensure_routine_execute_privilege(
        &self,
        definition: &CreateFunction,
    ) -> Result<(), SQLError> {
        let current = self.current_user_name();
        let allowed = self.current_user_is_superuser()
            || current == definition.owner
            || definition.execute_acl.as_ref().is_none_or(|acl| {
                acl.iter()
                    .any(|entry| entry.role == "PUBLIC" || entry.role == current)
            });
        if allowed {
            Ok(())
        } else {
            Err(SQLError::Routine {
                sqlstate: "42501".into(),
                message: format!(
                    "permission denied for {} {}",
                    routine_kind(definition),
                    routine_signature_label(&definition.name, &routine_signature_types(definition))
                ),
            })
        }
    }

    pub(super) fn ensure_routine_owner_as(
        definition: &CreateFunction,
        current_user: &str,
        current_user_is_superuser: bool,
    ) -> Result<(), SQLError> {
        if current_user_is_superuser || current_user == definition.owner {
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
