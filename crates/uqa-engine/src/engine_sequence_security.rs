//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL role ownership and owner-only sequence administration.

use crate::engine_roles::role_can_set;
use crate::{Engine, RelationIdentity, SQLError};

impl Engine {
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
        if persistence != uqa_sql::ast::RelationPersistence::Temporary {
            if let Some(catalog) = self.storage.catalog.as_ref() {
                let row = Self::sequence_row(name, object_id, state, persistence, &new_owner)
                    .map_err(|error| {
                        SQLError::Internal(format!("build sequence catalog row: {error}"))
                    })?;
                if !catalog.replace_sequence_row(&row).map_err(|error| {
                    SQLError::Internal(format!("persist sequence owner: {error}"))
                })? {
                    return Err(SQLError::Internal(format!(
                        "sequence `{name}` disappeared during owner change"
                    )));
                }
            }
        }
        self.durable
            .sequence_security
            .write()
            .get_mut(relation)
            .expect("preflighted sequence security must exist")
            .role_owner = new_owner;
        drop(memberships);
        drop(roles);
        self.note_catalog_registry_changed();
        Ok(())
    }
}
