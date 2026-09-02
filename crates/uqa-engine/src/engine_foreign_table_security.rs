//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable foreign-table ownership and access-control policy.

use super::{Engine, RelationIdentity, SQLError};
use crate::engine_capabilities::RelationResolution;
use crate::engine_roles::role_can_set;
use crate::engine_schema_security::SchemaAclPrivilege;
use crate::engine_state::TableSecurity;

impl Engine {
    fn bound_foreign_table_security(
        &self,
        name: &str,
    ) -> Result<(RelationIdentity, TableSecurity), SQLError> {
        let relation = RelationIdentity::from_legacy_name(name).map_err(|error| {
            SQLError::Internal(format!("resolve foreign table `{name}`: {error}"))
        })?;
        if !self.durable.foreign_tables.read().contains_key(&relation) {
            return Err(SQLError::Internal(format!(
                "foreign table `{name}` disappeared"
            )));
        }
        let security = self
            .durable
            .foreign_table_security
            .read()
            .get(&relation)
            .cloned()
            .ok_or_else(|| {
                SQLError::Internal(format!(
                    "foreign table `{name}` has no loaded security metadata"
                ))
            })?;
        Ok((relation, security))
    }

    pub(crate) fn ensure_foreign_table_owner(&self, name: &str) -> Result<String, SQLError> {
        let (relation, security) = self.bound_foreign_table_security(name)?;
        if self.current_user_has_role_privileges(&security.role_owner) {
            return Ok(security.role_owner);
        }
        Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: format!("must be owner of foreign table {}", relation.name),
        })
    }

    pub(crate) fn ensure_foreign_table_drop_authority(&self, name: &str) -> Result<(), SQLError> {
        let (relation, security) = self.bound_foreign_table_security(name)?;
        if self.current_user_has_role_privileges(&security.role_owner)
            || self
                .schema_security_for_privilege(&relation.schema)
                .is_some_and(|schema| self.current_user_has_role_privileges(&schema.role_owner))
        {
            return Ok(());
        }
        Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: format!("must be owner of foreign table {}", relation.name),
        })
    }

    pub(crate) fn persist_foreign_table_security(
        &self,
        relation: &RelationIdentity,
        security: &TableSecurity,
    ) -> Result<(), SQLError> {
        let Some(catalog) = self.storage.catalog.as_ref() else {
            return Ok(());
        };
        let updated = catalog
            .update_foreign_table_security(
                relation,
                &security.role_owner,
                security.acl.as_deref(),
                &security.column_acls,
            )
            .map_err(|error| {
                SQLError::Internal(format!(
                    "persist foreign table security for `{}`: {error}",
                    relation.qualified_name()
                ))
            })?;
        if !updated {
            return Err(SQLError::Internal(format!(
                "foreign table `{}` disappeared from durable catalog before security update",
                relation.qualified_name()
            )));
        }
        Ok(())
    }

    fn alter_foreign_table_role_owner(
        &self,
        name: &str,
        requested_owner: &str,
    ) -> Result<(), SQLError> {
        self.prepare_explicit_transaction_writer()?;
        let (relation, mut security) = self.bound_foreign_table_security(name)?;
        let current_owner = self.ensure_foreign_table_owner(name)?;
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
        crate::engine_table_security::rewrite_acl_owner(&mut security, &new_owner);
        let columns = self
            .durable
            .foreign_tables
            .read()
            .get(&relation)
            .map(|table| {
                table
                    .columns
                    .iter()
                    .map(|column| column.name.clone())
                    .collect::<Vec<_>>()
            })
            .ok_or_else(|| {
                SQLError::Internal(format!("foreign table `{name}` disappeared before update"))
            })?;
        crate::engine_table_security::validate_table_security_invariants(
            &security,
            Some(&columns),
            &self.durable.roles.read(),
        )
        .map_err(|error| {
            SQLError::Internal(format!(
                "foreign table `{name}` produced invalid ownership metadata after owner transfer: {error}"
            ))
        })?;
        self.persist_foreign_table_security(&relation, &security)?;
        self.durable
            .foreign_table_security
            .write()
            .insert(relation, security);
        self.note_catalog_registry_changed();
        Ok(())
    }

    pub(crate) fn alter_foreign_table(
        &self,
        statement: &uqa_sql::ast::AlterForeignTableStmt,
    ) -> Result<(), SQLError> {
        self.with_implicit_transaction(|engine| {
            let canonical = match engine.resolve_visible_relation_kind(&statement.name)? {
                RelationResolution::Found(canonical, "foreign table") => canonical,
                RelationResolution::Found(_, _) => {
                    return Err(SQLError::Routine {
                        sqlstate: "42809".into(),
                        message: format!("\"{}\" is not a foreign table", statement.name),
                    });
                }
                RelationResolution::MissingSchema(schema) if statement.if_exists => {
                    engine.push_sql_notice(
                        "NOTICE",
                        &format!("schema \"{schema}\" does not exist, skipping"),
                    );
                    return Ok(());
                }
                RelationResolution::MissingRelation if statement.if_exists => {
                    engine.push_sql_notice(
                        "NOTICE",
                        &format!(
                            "foreign table \"{}\" does not exist, skipping",
                            statement.name
                        ),
                    );
                    return Ok(());
                }
                RelationResolution::MissingSchema(schema) => {
                    return Err(SQLError::Routine {
                        sqlstate: "3F000".into(),
                        message: format!("schema \"{schema}\" does not exist"),
                    });
                }
                RelationResolution::MissingRelation => {
                    return Err(SQLError::Routine {
                        sqlstate: "42P01".into(),
                        message: format!("foreign table \"{}\" does not exist", statement.name),
                    });
                }
            };
            engine.lock_relation(
                &canonical,
                crate::row_locks::RelationLockMode::AccessExclusive,
            )?;
            engine.alter_foreign_table_role_owner(&canonical, &statement.owner)
        })
    }
}
