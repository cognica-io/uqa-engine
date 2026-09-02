//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable ordinary-table ownership and owner-authorized lifecycle changes.

use std::sync::Arc;

use uqa_sql::SQLError;

use crate::engine_roles::role_can_set;
use crate::engine_schema_security::SchemaAclPrivilege;
use crate::engine_state::SequenceSecurity;
use crate::{Engine, RelationIdentity, TableState};

impl Engine {
    fn bound_table_for_security(
        &self,
        name: &str,
    ) -> Result<(RelationIdentity, Arc<TableState>), SQLError> {
        let relation = RelationIdentity::from_legacy_name(name)
            .map_err(|error| SQLError::Internal(format!("resolve table `{name}`: {error}")))?;
        let table = self
            .storage
            .tables
            .read()
            .get(&relation)
            .cloned()
            .ok_or_else(|| SQLError::Internal(format!("table `{name}` disappeared")))?;
        Ok((relation, table))
    }

    pub(crate) fn ensure_table_owner(&self, name: &str) -> Result<String, SQLError> {
        let (relation, table) = self.bound_table_for_security(name)?;
        let owner = table.role_owner();
        if self.current_user_has_role_privileges(&owner) {
            return Ok(owner);
        }
        Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: format!("must be owner of table {}", relation.name),
        })
    }

    pub(crate) fn ensure_table_drop_authority(&self, name: &str) -> Result<(), SQLError> {
        let (relation, table) = self.bound_table_for_security(name)?;
        let table_owner = table.role_owner();
        if self.current_user_has_role_privileges(&table_owner) {
            return Ok(());
        }
        if self
            .schema_security_for_privilege(&relation.schema)
            .is_some_and(|security| self.current_user_has_role_privileges(&security.role_owner))
        {
            return Ok(());
        }
        Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: format!("must be owner of table {}", relation.name),
        })
    }

    pub(crate) fn alter_table_role_owner(
        &self,
        name: &str,
        requested_owner: &str,
    ) -> Result<(), SQLError> {
        self.prepare_explicit_transaction_writer()?;
        let (relation, table) = self.bound_table_for_security(name)?;
        let current_owner = self.ensure_table_owner(name)?;
        let new_owner = self.resolve_role_reference(requested_owner);
        let current_user_is_superuser;
        {
            let roles = self.durable.roles.read();
            if !roles.contains_key(&new_owner) {
                return Err(SQLError::Routine {
                    sqlstate: "42704".into(),
                    message: format!("role \"{new_owner}\" does not exist"),
                });
            }
            let memberships = self.durable.role_memberships.read();
            let current_user = self.current_user_name();
            current_user_is_superuser = roles
                .get(&current_user)
                .is_some_and(|role| role.has(uqa_sql::ast::RoleAttribute::Superuser));
            if !role_can_set(&roles, &memberships, &current_user, &new_owner) {
                return Err(SQLError::Routine {
                    sqlstate: "42501".into(),
                    message: format!("must be able to SET ROLE \"{new_owner}\""),
                });
            }
        }
        if current_owner == new_owner {
            return Ok(());
        }
        if !current_user_is_superuser {
            self.require_schema_privilege(
                &relation.schema,
                &new_owner,
                SchemaAclPrivilege::Create,
            )?;
        }

        let sequence_updates =
            self.table_owned_sequence_owner_updates(table.object_id(), &new_owner)?;
        for (sequence, security) in &sequence_updates {
            self.persist_sequence_security(&sequence.qualified_name(), sequence, security)?;
        }
        let columns = table.columns.read().clone();
        let constraints = uqa_sql::ast::TableConstraintSet {
            checks: table.table_checks.read().clone(),
            foreign_keys: table.foreign_keys.read().clone(),
            key_constraints: table.key_constraints.read().clone(),
            persistence: table.persistence,
            on_commit: table.on_commit,
            hierarchy: table.hierarchy.read().clone(),
        };
        self.try_save_table_schema_with_components_and_owner(
            name,
            &table,
            &columns,
            &constraints,
            &new_owner,
        )
        .map_err(|error| SQLError::Internal(format!("persist table owner: {error}")))?;

        table.role_owner.write().clone_from(&new_owner);
        if !sequence_updates.is_empty() {
            let mut registry = self.durable.sequence_security.write();
            for (sequence, security) in sequence_updates {
                registry.insert(sequence, security);
            }
            drop(registry);
            self.note_catalog_registry_changed();
        }
        if table.persistence == uqa_sql::ast::RelationPersistence::Temporary {
            self.note_table_catalog_changed();
        }
        Ok(())
    }

    fn table_owned_sequence_owner_updates(
        &self,
        table_object_id: [u8; 16],
        new_owner: &str,
    ) -> Result<Vec<(RelationIdentity, SequenceSecurity)>, SQLError> {
        let owned = self
            .durable
            .sequences
            .read()
            .iter()
            .filter_map(|(relation, state)| {
                state
                    .owner
                    .is_some_and(|owner| owner.table_object_id == table_object_id)
                    .then_some(relation.clone())
            })
            .collect::<Vec<_>>();
        let registry = self.durable.sequence_security.read();
        let mut updates = Vec::with_capacity(owned.len());
        for relation in owned {
            let mut security = registry.get(&relation).cloned().ok_or_else(|| {
                SQLError::Internal(format!(
                    "sequence `{}` has no security metadata",
                    relation.qualified_name()
                ))
            })?;
            Self::rewrite_sequence_security_owner(&mut security, new_owner);
            updates.push((relation, security));
        }
        Ok(updates)
    }
}
