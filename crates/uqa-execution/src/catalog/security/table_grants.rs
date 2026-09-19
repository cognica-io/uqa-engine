//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Apply table-shaped privilege commands while preserving authorization guards and persistence/publication order.
pub mod context;
mod locking;
mod targets;
mod updates;
pub use context::{TableGrantContext, TableGrantInputs};
mod prepared;
use super::roles::{
    dependencies::{prepare_role_dependencies, RoleDependencyCandidate},
    locking::RoleLockContext,
};
use updates::persist_table_privilege_updates;
use uqa_sql::catalog::security::acl_command::AclCommandRoles;
use uqa_sql::{
    ast::{
        GrantSequenceStmt, GrantSequenceTarget, GrantTableStmt, SequenceRevokeBehavior,
        TableRevokeBehavior,
    },
    catalog::security::table_grants::{table_sequence_privileges, ResolvedTableGrantTarget},
    SQLError,
};
pub fn grant_table_privileges(
    inputs: &dyn TableGrantInputs,
    statement: &GrantTableStmt,
) -> Result<(), SQLError> {
    inputs
        .table_grant_context()
        .grant_table_privileges(statement)
}
impl TableGrantContext<'_> {
    pub fn grant_table_privileges(&self, statement: &GrantTableStmt) -> Result<(), SQLError> {
        let targets = locking::lock_targets(self, statement)?;
        let role_locks = RoleLockContext {
            roles: self.roles,
            session: self.shared_locks,
        };
        let mut command_roles = AclCommandRoles::default();
        let RoleDependencyCandidate {
            roles,
            memberships,
            value:
                prepared::PreparedTableGrant {
                    updates,
                    view_updates,
                    foreign_updates,
                    system_updates,
                    notices,
                },
            ..
        } = prepare_role_dependencies(
            &role_locks,
            || self.writer.prepare_writer(),
            || prepared::prepare(self, statement, &targets, &mut command_roles),
        )?;
        persist_table_privilege_updates(self, &updates, &view_updates, &foreign_updates)?;
        for update in &system_updates {
            update.persist(self.catalog).map_err(|error| {
                SQLError::Internal(format!("persist system relation privileges: {error}"))
            })?;
        }
        let table_changed = !updates.is_empty();
        for (_, table, security) in updates {
            table.security_write().clone_from(&security);
        }
        let view_changed = !view_updates.is_empty();
        if view_changed {
            let mut views = self.views.views_write();
            for (relation, view) in view_updates {
                views.insert(relation, view);
            }
        }
        let foreign_changed = !foreign_updates.is_empty();
        if foreign_changed {
            let mut securities = self.foreign.security_write();
            for (relation, security) in foreign_updates {
                securities.insert(relation, security);
            }
        }
        let system_changed = !system_updates.is_empty();
        if system_changed {
            let mut securities = self.system.system_relation_securities_write();
            for update in system_updates {
                update.publish(&mut securities);
            }
        }
        drop(memberships);
        drop(roles);

        self.grant_table_syntax_sequence_privileges(statement, &targets, &mut command_roles)?;
        for (level, message) in notices {
            self.notices.notice(level, &message);
        }
        if table_changed || view_changed || foreign_changed || system_changed {
            self.changes.table_catalog_changed();
            self.changes.catalog_registry_changed();
        }
        Ok(())
    }
    fn grant_table_syntax_sequence_privileges(
        &self,
        statement: &GrantTableStmt,
        targets: &[ResolvedTableGrantTarget],
        command_roles: &mut AclCommandRoles,
    ) -> Result<(), SQLError> {
        let sequence_names = targets
            .iter()
            .filter(|target| target.kind == "sequence")
            .map(|target| target.name.clone())
            .collect::<Vec<_>>();
        if !sequence_names.is_empty() {
            let (sequence_privileges, has_inapplicable) =
                table_sequence_privileges(&statement.privileges);
            if has_inapplicable {
                for target in targets.iter().filter(|target| target.kind == "sequence") {
                    self.notices.notice(
                        "WARNING",
                        &format!(
                            "sequence \"{}\" only supports USAGE, SELECT, and UPDATE privileges",
                            target.relation.name
                        ),
                    );
                }
            }
            if !sequence_privileges.is_empty() {
                self.sequences.grant_sequence_privileges_with_roles(
                    &GrantSequenceStmt {
                        is_grant: statement.is_grant,
                        grant_option: statement.grant_option,
                        grant_option_only: statement.grant_option_only,
                        privileges: sequence_privileges,
                        target: GrantSequenceTarget::Sequences {
                            names: sequence_names,
                        },
                        grantees: statement.grantees.clone(),
                        grantor: statement.grantor.clone(),
                        revoke_behavior: if statement.revoke_behavior
                            == TableRevokeBehavior::Cascade
                        {
                            SequenceRevokeBehavior::Cascade
                        } else {
                            SequenceRevokeBehavior::Restrict
                        },
                    },
                    command_roles,
                )?;
            }
        }
        Ok(())
    }
}
