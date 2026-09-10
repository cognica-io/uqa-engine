//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Schema ownership transfer with inherited authority and ACL identity preservation.

use crate::engine_database_security::DatabaseAclPrivilege;
use crate::engine_roles::role_can_set;
use crate::engine_state::SchemaSecurity;
use crate::{Engine, SQLError};

impl Engine {
    pub(crate) fn alter_schema_owner(&self, name: &str, requested: &str) -> Result<(), SQLError> {
        self.prepare_explicit_transaction_writer()?;
        self.synchronize_catalog_registries()
            .map_err(|error| SQLError::Internal(error.to_string()))?;
        let new_owner = self.resolve_role_reference(requested);
        if !self.durable.roles.read().contains_key(&new_owner) {
            return Err(SQLError::Routine {
                sqlstate: "42704".into(),
                message: format!("role \"{new_owner}\" does not exist"),
            });
        }
        let mut security =
            self.schema_security_for_privilege(name)
                .ok_or_else(|| SQLError::Routine {
                    sqlstate: "3F000".into(),
                    message: format!("schema \"{name}\" does not exist"),
                })?;
        if security.role_owner == new_owner {
            return Ok(());
        }
        if !self.current_user_has_role_privileges(&security.role_owner) {
            return Err(SQLError::Routine {
                sqlstate: "42501".into(),
                message: format!("must be owner of schema {name}"),
            });
        }
        if !self.current_user_is_superuser() {
            if !role_can_set(
                &self.durable.roles.read(),
                &self.durable.role_memberships.read(),
                &self.current_user_name(),
                &new_owner,
            ) {
                return Err(SQLError::Routine {
                    sqlstate: "42501".into(),
                    message: format!("must be able to SET ROLE \"{new_owner}\""),
                });
            }
            self.ensure_database_privilege(&new_owner, DatabaseAclPrivilege::Create)?;
        }
        rewrite_schema_acl_owner(&mut security, &new_owner);
        self.persist_schema_security(name, &security)?;
        self.durable
            .schemas
            .write()
            .insert(name.to_string(), security);
        self.note_catalog_registry_changed();
        Ok(())
    }
}

fn rewrite_schema_acl_owner(security: &mut SchemaSecurity, new_owner: &str) {
    if let Some(acl) = &mut security.acl {
        for entry in acl.iter_mut() {
            if entry.role == security.role_owner {
                entry.role = new_owner.to_string();
            }
            if entry.grantor.as_deref().unwrap_or(&security.role_owner) == security.role_owner {
                entry.grantor = Some(new_owner.to_string());
            }
        }
        let mut merged: Vec<uqa_storage::SchemaAclEntry> = Vec::new();
        for entry in std::mem::take(acl) {
            if let Some(previous) = merged
                .iter_mut()
                .find(|previous| previous.role == entry.role && previous.grantor == entry.grantor)
            {
                previous.privileges.insert(entry.privileges);
                previous.grant_options.insert(entry.grant_options);
            } else {
                merged.push(entry);
            }
        }
        *acl = merged;
    }
    security.role_owner = new_owner.to_string();
}
