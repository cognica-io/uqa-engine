//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sequence ACL publication with retained authorization and registry guards.

use super::roles::{
    dependencies::{prepare_role_dependencies, RoleDependencyCandidate},
    locking::RoleLockContext,
};
use crate::catalog::{
    context::CatalogContext,
    projection::{resolve_regclass_kind_by_oid, sequence_relation_oid},
    sequence::sequence_row,
    sequence_introspection::SequenceIntrospectionCatalog,
};
use crate::row_locks::shared_objects::SharedObjectLockSession;
use std::{
    collections::{BTreeMap, BTreeSet},
    ops::DerefMut,
};
use uqa_core::RelationIdentity;
use uqa_sql::{
    ast::{GrantSequenceStmt, GrantSequenceTarget},
    catalog::{
        roles::resolve_role_specification,
        security::{
            dependencies::added_acl_roles,
            sequence::requested_acl_privileges,
            sequence_grants::{
                apply_sequence_acl, bind_named_sequence_grants, bind_sequence_grant_schemas,
                sequence_acl_warning, sequence_grants_in_schemas, validate_sequence_acl_roles,
                validate_sequence_grant_target_kinds, ResolvedSequenceGrantTarget,
                SequenceGrantNamespace,
            },
            sequence_inquiry::SequencePrivilegeInquiry,
            SequenceSecurity,
        },
    },
    SQLError,
};
use uqa_storage::{CatalogFacade, StorageBackendResult};

pub type SequenceSecurityWrite<'a> =
    Box<dyn DerefMut<Target = BTreeMap<RelationIdentity, SequenceSecurity>> + 'a>;
pub trait SequencePrivilegePublication {
    fn prepare_writer(&self) -> Result<(), SQLError>;
    fn refresh_catalog(&self) -> StorageBackendResult<()>;
    fn security_write(&self) -> SequenceSecurityWrite<'_>;
    fn catalog_changed(&self);
    fn notice(&self, level: &str, message: &str);
}
pub struct SequencePrivilegeContext<'a> {
    pub inquiry: SequencePrivilegeInquiry<'a>,
    pub locks: &'a dyn SharedObjectLockSession,
    pub sequences: &'a dyn SequenceIntrospectionCatalog,
    pub namespaces: &'a dyn SequenceGrantNamespace,
    pub publication: &'a dyn SequencePrivilegePublication,
    pub catalog: CatalogContext<'a>,
    pub storage: Option<&'a dyn CatalogFacade>,
}

struct SequencePrivilegeCandidate<'a> {
    registry: SequenceSecurityWrite<'a>,
    updates: Vec<(String, RelationIdentity, SequenceSecurity)>,
    notices: Vec<(&'static str, String)>,
}

impl SequencePrivilegeContext<'_> {
    pub fn grant_sequence_privileges(&self, statement: &GrantSequenceStmt) -> Result<(), SQLError> {
        let targets = self.resolve_sequence_grant_targets(&statement.target)?;
        let RoleDependencyCandidate {
            roles,
            memberships,
            value:
                SequencePrivilegeCandidate {
                    mut registry,
                    updates,
                    notices,
                },
            ..
        } = prepare_role_dependencies(
            &RoleLockContext {
                roles: self.inquiry.roles,
                session: self.locks,
            },
            || self.publication.prepare_writer(),
            || self.prepare_privileges(statement, &targets),
        )?;
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
            self.publication.notice(level, &message);
        }
        if changed {
            self.publication.catalog_changed();
        }
        Ok(())
    }

    fn prepare_privileges<'a>(
        &'a self,
        statement: &GrantSequenceStmt,
        targets: &[ResolvedSequenceGrantTarget],
    ) -> Result<RoleDependencyCandidate<'a, SequencePrivilegeCandidate<'a>>, SQLError> {
        let roles = self.inquiry.roles.role_definitions();
        let grantees = statement
            .grantees
            .iter()
            .map(|role| {
                uqa_sql::catalog::roles::resolve_acl_role_specification(
                    self.inquiry.names,
                    role,
                    &roles,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        let requested_grantor = statement
            .grantor
            .as_ref()
            .map(|role| resolve_role_specification(self.inquiry.names, role).catalog_name(&roles))
            .transpose()?;
        let current_user = self.inquiry.names.current_role();
        validate_sequence_acl_roles(
            statement,
            &grantees,
            requested_grantor.as_deref(),
            &current_user,
            &roles,
        )?;
        validate_sequence_grant_target_kinds(targets)?;
        let privileges = requested_acl_privileges(&statement.privileges)?;
        let memberships = self.inquiry.roles.role_memberships();
        let registry = self.publication.security_write();
        let mut updates = Vec::new();
        let mut notices = Vec::new();
        let mut dependencies = BTreeSet::new();
        for target in targets {
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
            added_acl_roles(
                current.acl.as_deref().unwrap_or_default(),
                &current.role_owner,
                next.acl.as_deref().unwrap_or_default(),
                &next.role_owner,
                &mut dependencies,
            );
            if next != current {
                updates.push((target.name.clone(), target.relation.clone(), next));
            }
        }
        Ok(RoleDependencyCandidate {
            value: SequencePrivilegeCandidate {
                registry,
                updates,
                notices,
            },
            memberships,
            roles,
            dependencies,
        })
    }

    fn resolve_sequence_grant_targets(
        &self,
        target: &GrantSequenceTarget,
    ) -> Result<Vec<ResolvedSequenceGrantTarget>, SQLError> {
        match target {
            GrantSequenceTarget::Sequences { names } => {
                bind_named_sequence_grants(self.inquiry.resolution, names)
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
        self.publication.refresh_catalog().map_err(|error| {
            SQLError::Internal(format!("load schemas for sequence privileges: {error}"))
        })?;
        self.sequences.refresh_sequences().map_err(|error| {
            SQLError::Internal(format!("load sequences for privileges: {error}"))
        })?;
        let schemas = bind_sequence_grant_schemas(self.namespaces, schemas)?;
        let sequences = self.sequences.states();
        let targets = sequence_grants_in_schemas(&schemas, sequences.keys());
        Ok(targets)
    }

    pub fn persist_sequence_security(
        &self,
        name: &str,
        relation: &RelationIdentity,
        security: &SequenceSecurity,
    ) -> Result<(), SQLError> {
        let state = self
            .sequences
            .sequence_state(relation)
            .ok_or_else(|| SQLError::Internal(format!("sequence `{name}` disappeared")))?;
        let object_id = self
            .sequences
            .object_ids()
            .get(relation)
            .copied()
            .ok_or_else(|| {
                SQLError::Internal(format!("sequence `{name}` has no object identity"))
            })?;
        let persistence = self
            .sequences
            .sequence_persistence(relation)
            .ok_or_else(|| {
                SQLError::Internal(format!("sequence `{name}` has no persistence metadata"))
            })?;
        if persistence == uqa_sql::ast::RelationPersistence::Temporary {
            return Ok(());
        }
        let Some(catalog) = self.storage else {
            return Ok(());
        };
        let row = sequence_row(name, object_id, state, persistence, security)
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

    pub fn resolve_sequence_privilege_oid(
        &self,
        oid: i64,
    ) -> Result<Option<(String, RelationIdentity)>, SQLError> {
        self.sequences.refresh_sequences().map_err(|error| {
            SQLError::Internal(format!("load sequences for privilege inquiry: {error}"))
        })?;
        if let Some((relation, _)) = self
            .sequences
            .object_ids()
            .iter()
            .find(|(_, object_id)| sequence_relation_oid(**object_id) == oid)
        {
            return Ok(Some((relation.qualified_name(), relation.clone())));
        }
        if let Some((name, _kind)) = resolve_regclass_kind_by_oid(&self.catalog, oid)? {
            return Err(SQLError::Routine {
                sqlstate: "42809".into(),
                message: format!("\"{name}\" is not a sequence"),
            });
        }
        Ok(None)
    }
}
