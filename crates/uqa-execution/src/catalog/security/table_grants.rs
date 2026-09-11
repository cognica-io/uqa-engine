//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Apply table-shaped privilege commands while preserving authorization guards and persistence/publication order.
pub mod context;
mod targets;
mod updates;
pub use context::{TableGrantContext, TableGrantInputs};
use targets::{
    validated_foreign_table_grant_targets, validated_table_grant_targets,
    validated_view_grant_targets,
};
use updates::{persist_table_privilege_updates, table_privilege_updates};
use uqa_sql::{
    ast::{
        GrantSequenceStmt, GrantSequenceTarget, GrantTableStmt, GrantTableTarget,
        SequenceRevokeBehavior, TableRevokeBehavior,
    },
    catalog::{
        roles::resolve_role_reference,
        security::{
            table::{requested_acl_privileges, RequestedTablePrivileges},
            table_grants::{
                foreign_table_privilege_updates, table_sequence_privileges,
                validate_table_acl_roles, validate_table_grant_target_kinds,
                view_privilege_updates, ResolvedTableGrantTarget, TableGrantApplication,
            },
        },
    },
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
        self.writer.prepare_writer()?;
        let targets = self.resolve_table_grant_targets(&statement.target)?;
        let grantees = statement
            .grantees
            .iter()
            .map(|role| resolve_role_reference(self.names, role))
            .collect::<Vec<_>>();
        let requested_grantor = statement
            .grantor
            .as_ref()
            .map(|role| resolve_role_reference(self.names, role));
        let current_user = self.names.current_user_name();
        let roles = self.roles.role_definitions();
        validate_table_acl_roles(
            statement,
            &grantees,
            requested_grantor.as_deref(),
            &current_user,
            &roles,
        )?;

        validate_table_grant_target_kinds(statement, &targets)?;
        let has_table_relations = targets.iter().any(|target| {
            matches!(
                target.kind,
                "table" | "view" | "materialized view" | "foreign table"
            )
        });
        let requested_privileges = if has_table_relations
            || matches!(
                statement.target,
                GrantTableTarget::AllTablesInSchemas { .. }
            ) {
            requested_acl_privileges(&statement.privileges)?
        } else {
            RequestedTablePrivileges {
                table: Vec::new(),
                columns: Vec::new(),
            }
        };
        let memberships = self.roles.role_memberships();
        let table_targets = validated_table_grant_targets(self, &targets, &requested_privileges)?;
        let view_targets = validated_view_grant_targets(self, &targets, &requested_privileges)?;
        let foreign_targets =
            validated_foreign_table_grant_targets(self, &targets, &requested_privileges)?;
        let mut notices = Vec::new();
        let application = TableGrantApplication {
            statement,
            grantees: &grantees,
            requested: &requested_privileges,
            current_user: &current_user,
            roles: &roles,
            memberships: &memberships,
        };
        let updates = table_privilege_updates(table_targets, &application, &mut notices)?;
        let view_updates = view_privilege_updates(view_targets, &application, &mut notices)?;
        let foreign_updates =
            foreign_table_privilege_updates(foreign_targets, &application, &mut notices)?;
        persist_table_privilege_updates(self, &updates, &view_updates, &foreign_updates)?;
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
        drop(memberships);
        drop(roles);

        self.grant_table_syntax_sequence_privileges(statement, &targets)?;
        for (level, message) in notices {
            self.notices.notice(level, &message);
        }
        if table_changed || view_changed || foreign_changed {
            self.changes.table_catalog_changed();
            self.changes.catalog_registry_changed();
        }
        Ok(())
    }
    fn grant_table_syntax_sequence_privileges(
        &self,
        statement: &GrantTableStmt,
        targets: &[ResolvedTableGrantTarget],
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
                self.sequences
                    .grant_sequence_privileges(&GrantSequenceStmt {
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
                    })?;
            }
        }
        Ok(())
    }
}
