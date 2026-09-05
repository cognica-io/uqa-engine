//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable table-shaped relation ownership, access-control lists, and authorization checks.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use uqa_sql::ast::{
    GrantSequenceStmt, GrantSequenceTarget, GrantTableStmt, GrantTableTarget,
    SequenceRevokeBehavior, TableRevokeBehavior,
};
use uqa_sql::SQLError;
use uqa_storage::TableAclEntry;

mod acl;
mod columns;
mod grants;
mod inquiry;

use acl::{
    requested_acl_privileges, role_has_privilege, RequestedTablePrivileges, TablePrivilegeCheck,
};
pub(crate) use acl::{rewrite_acl_owner, TableAclPrivilege};
use columns::{column_grant_option_roles, role_has_column_privilege as column_privilege_check};
use grants::{
    foreign_table_privilege_updates, persist_table_privilege_updates, table_privilege_updates,
    table_sequence_privileges, validate_table_acl_roles, validate_table_grant_target_kinds,
    validated_foreign_table_grant_targets, validated_table_grant_targets,
    validated_view_grant_targets, view_privilege_updates, ResolvedTableGrantTarget,
    TableGrantApplication,
};

use crate::engine_capabilities::RelationResolution;
use crate::engine_roles::{role_can_set, RoleDefinition, RoleMembership, RoleMembershipKey};
use crate::engine_schema_security::SchemaAclPrivilege;
use crate::engine_state::{SequenceSecurity, TableSecurity};
use crate::{Engine, RelationIdentity, TableState};

enum ResolvedTablePrivilegeTarget {
    Table(RelationIdentity),
    View(RelationIdentity),
    ForeignTable(RelationIdentity),
    Sequence(RelationIdentity),
}

enum ResolvedColumnPrivilegeTarget {
    User(String),
    System,
}

const POSTGRES_SYSTEM_COLUMNS: [&str; 6] = ["ctid", "xmin", "cmin", "xmax", "cmax", "tableoid"];

pub(crate) fn role_can_view_table(
    security: &TableSecurity,
    subject: &str,
    roles: &BTreeMap<String, RoleDefinition>,
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
) -> bool {
    TableAclPrivilege::ALL.into_iter().any(|privilege| {
        role_has_privilege(
            security,
            subject,
            TablePrivilegeCheck {
                privilege,
                grant_option: false,
            },
            roles,
            memberships,
        )
    }) || security.column_acls.iter().any(|(column, _)| {
        TableAclPrivilege::COLUMN_ALL.into_iter().any(|privilege| {
            column_privilege_check(
                security,
                column,
                subject,
                TablePrivilegeCheck {
                    privilege,
                    grant_option: false,
                },
                roles,
                memberships,
            )
        })
    })
}

pub(crate) fn role_has_table_privilege(
    security: &TableSecurity,
    subject: &str,
    privilege: TableAclPrivilege,
    roles: &BTreeMap<String, RoleDefinition>,
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
) -> bool {
    role_has_privilege(
        security,
        subject,
        TablePrivilegeCheck {
            privilege,
            grant_option: false,
        },
        roles,
        memberships,
    )
}

pub(crate) fn role_has_column_privilege(
    security: &TableSecurity,
    column: &str,
    subject: &str,
    privilege: TableAclPrivilege,
    roles: &BTreeMap<String, RoleDefinition>,
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
) -> bool {
    column_privilege_check(
        security,
        column,
        subject,
        TablePrivilegeCheck {
            privilege,
            grant_option: false,
        },
        roles,
        memberships,
    )
}

pub(crate) fn validate_table_security_invariants(
    security: &TableSecurity,
    columns: Option<&[String]>,
    roles: &BTreeMap<String, RoleDefinition>,
) -> Result<(), String> {
    let validate_acl = |acl: &[TableAclEntry], column: Option<&str>| -> Result<(), String> {
        let mut paths = BTreeSet::new();
        for entry in acl {
            let grantor = acl::acl_grantor(entry, &security.role_owner);
            if entry.role != "PUBLIC" && !roles.contains_key(&entry.role) {
                return Err(format!(
                    "ACL references missing grantee role `{}`",
                    entry.role
                ));
            }
            if grantor == "PUBLIC" || !roles.contains_key(grantor) {
                return Err(format!("ACL references missing grantor role `{grantor}`"));
            }
            if !paths.insert((entry.role.as_str(), grantor)) {
                return Err(format!(
                    "ACL contains duplicate grant path `{grantor}` -> `{}`",
                    entry.role
                ));
            }
            if entry.privileges.is_empty() && entry.grant_options.is_empty() {
                return Err("ACL contains an empty grant path".into());
            }
            if entry.role == "PUBLIC" && !entry.grant_options.is_empty() {
                return Err("PUBLIC cannot hold grant options".into());
            }
            for privilege in TableAclPrivilege::ALL {
                let mask = privilege.mask();
                if entry.grant_options.intersects(mask) && !entry.privileges.intersects(mask) {
                    return Err("ACL grant option exists without its privilege".into());
                }
                if entry.privileges.intersects(mask) || entry.grant_options.intersects(mask) {
                    let reachable = column.map_or_else(
                        || acl::grant_option_roles(security, privilege),
                        |column| column_grant_option_roles(security, column, privilege),
                    );
                    if !reachable.contains(grantor) {
                        return Err(format!(
                            "ACL grant path from `{grantor}` is not rooted at owner `{}`",
                            security.role_owner
                        ));
                    }
                }
            }
        }
        Ok(())
    };

    if let Some(acl) = security.acl.as_deref() {
        validate_acl(acl, None)?;
    }
    if !security.column_acls.is_empty() && columns.is_none() {
        return Err("column ACLs require durable public column metadata".into());
    }
    for (column, acl) in &security.column_acls {
        if !columns.is_some_and(|columns| columns.iter().any(|candidate| candidate == column)) {
            return Err(format!("column ACL references missing column `{column}`"));
        }
        for entry in acl {
            if entry.privileges.delete
                || entry.privileges.truncate
                || entry.privileges.trigger
                || entry.privileges.maintain
                || entry.grant_options.delete
                || entry.grant_options.truncate
                || entry.grant_options.trigger
                || entry.grant_options.maintain
            {
                return Err(format!(
                    "column ACL for `{column}` contains a relation-only privilege"
                ));
            }
        }
        validate_acl(acl, Some(column))?;
    }
    Ok(())
}

impl Engine {
    pub(crate) fn grant_table_privileges(
        &self,
        statement: &GrantTableStmt,
    ) -> Result<(), SQLError> {
        self.prepare_explicit_transaction_writer()?;
        let targets = self.resolve_table_grant_targets(&statement.target)?;
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
        let memberships = self.durable.role_memberships.read();
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
            table.security.write().clone_from(&security);
        }
        let view_changed = !view_updates.is_empty();
        if view_changed {
            let mut views = self.durable.views.write();
            for (relation, view) in view_updates {
                views.insert(relation, view);
            }
        }
        let foreign_changed = !foreign_updates.is_empty();
        if foreign_changed {
            let mut securities = self.durable.foreign_table_security.write();
            for (relation, security) in foreign_updates {
                securities.insert(relation, security);
            }
        }
        drop(memberships);
        drop(roles);

        self.grant_table_syntax_sequence_privileges(statement, &targets)?;
        for (level, message) in notices {
            self.push_sql_notice(level, &message);
        }
        if table_changed || view_changed || foreign_changed {
            self.note_table_catalog_changed();
            self.note_catalog_registry_changed();
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
                    self.push_sql_notice(
                        "WARNING",
                        &format!(
                            "sequence \"{}\" only supports USAGE, SELECT, and UPDATE privileges",
                            target.relation.name
                        ),
                    );
                }
            }
            if !sequence_privileges.is_empty() {
                self.grant_sequence_privileges(&GrantSequenceStmt {
                    is_grant: statement.is_grant,
                    grant_option: statement.grant_option,
                    grant_option_only: statement.grant_option_only,
                    privileges: sequence_privileges,
                    target: GrantSequenceTarget::Sequences {
                        names: sequence_names,
                    },
                    grantees: statement.grantees.clone(),
                    grantor: statement.grantor.clone(),
                    revoke_behavior: if statement.revoke_behavior == TableRevokeBehavior::Cascade {
                        SequenceRevokeBehavior::Cascade
                    } else {
                        SequenceRevokeBehavior::Restrict
                    },
                })?;
            }
        }
        Ok(())
    }

    fn resolve_table_grant_targets(
        &self,
        target: &GrantTableTarget,
    ) -> Result<Vec<ResolvedTableGrantTarget>, SQLError> {
        match target {
            GrantTableTarget::Relations { names } => {
                let mut resolved = Vec::with_capacity(names.len());
                for requested in names {
                    let (name, kind) = match self.resolve_visible_relation_kind(requested)? {
                        RelationResolution::Found(name, kind) => (name, kind),
                        RelationResolution::MissingSchema(schema) => {
                            return Err(SQLError::Routine {
                                sqlstate: "3F000".into(),
                                message: format!("schema \"{schema}\" does not exist"),
                            })
                        }
                        RelationResolution::MissingRelation => {
                            return Err(SQLError::Routine {
                                sqlstate: "42P01".into(),
                                message: format!("relation \"{requested}\" does not exist"),
                            })
                        }
                    };
                    let relation = Self::resolved_relation_identity(&name).map_err(|error| {
                        SQLError::Internal(format!("resolve table `{name}`: {error}"))
                    })?;
                    resolved.push(ResolvedTableGrantTarget {
                        requested: requested.clone(),
                        name,
                        relation,
                        kind,
                    });
                }
                Ok(resolved)
            }
            GrantTableTarget::AllTablesInSchemas { schemas } => {
                self.resolve_all_tables_in_schemas(schemas)
            }
        }
    }

    fn resolve_all_tables_in_schemas(
        &self,
        schemas: &[String],
    ) -> Result<Vec<ResolvedTableGrantTarget>, SQLError> {
        self.synchronize_catalog_registries().map_err(|error| {
            SQLError::Internal(format!("load schemas for table privileges: {error}"))
        })?;
        self.synchronize_table_catalog()
            .map_err(|error| SQLError::Internal(format!("load tables for privileges: {error}")))?;
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
        let tables = self.storage.tables.read();
        let mut targets = tables
            .keys()
            .filter(|relation| resolved_schemas.contains(&relation.schema))
            .map(|relation| ResolvedTableGrantTarget {
                requested: relation.qualified_name(),
                name: relation.qualified_name(),
                relation: relation.clone(),
                kind: "table",
            })
            .collect::<Vec<_>>();
        drop(tables);
        targets.extend(
            self.durable
                .views
                .read()
                .iter()
                .filter(|(relation, _)| resolved_schemas.contains(&relation.schema))
                .map(|(relation, view)| ResolvedTableGrantTarget {
                    requested: relation.qualified_name(),
                    name: relation.qualified_name(),
                    relation: relation.clone(),
                    kind: match view.kind {
                        crate::StoredViewKind::View => "view",
                        crate::StoredViewKind::Materialized => "materialized view",
                    },
                }),
        );
        targets.extend(
            self.durable
                .foreign_tables
                .read()
                .keys()
                .filter(|relation| resolved_schemas.contains(&relation.schema))
                .map(|relation| ResolvedTableGrantTarget {
                    requested: relation.qualified_name(),
                    name: relation.qualified_name(),
                    relation: relation.clone(),
                    kind: "foreign table",
                }),
        );
        targets.sort_by(|left, right| left.relation.cmp(&right.relation));
        Ok(targets)
    }

    fn persist_table_security(
        &self,
        name: &str,
        table: &TableState,
        security: &TableSecurity,
    ) -> Result<(), SQLError> {
        let columns = table.columns.read().clone();
        let constraints = uqa_sql::ast::TableConstraintSet {
            checks: table.table_checks.read().clone(),
            foreign_keys: table.foreign_keys.read().clone(),
            key_constraints: table.key_constraints.read().clone(),
            persistence: table.persistence,
            on_commit: table.on_commit,
            hierarchy: table.hierarchy.read().clone(),
        };
        self.try_save_table_schema_with_components_and_security(
            name,
            table,
            &columns,
            &constraints,
            security,
        )
        .map_err(|error| SQLError::Internal(format!("persist table privileges: {error}")))
    }

    pub(crate) fn ensure_table_privilege(
        &self,
        name: &str,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        let current_user = self.current_user_name();
        self.ensure_table_privilege_for(name, &current_user, privilege)
    }

    pub(crate) fn ensure_table_privilege_for(
        &self,
        name: &str,
        subject: &str,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        let (relation, table) = self.bound_table_for_security(name)?;
        let security = table.security();
        let roles = self.durable.roles.read();
        let memberships = self.durable.role_memberships.read();
        if role_has_privilege(
            &security,
            subject,
            TablePrivilegeCheck {
                privilege,
                grant_option: false,
            },
            &roles,
            &memberships,
        ) {
            return Ok(());
        }
        Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: format!("permission denied for table {}", relation.name),
        })
    }

    /// Return declared columns for a mutation target whose canonical relation identity has already been authorized and bound.
    pub(crate) fn bound_table_column_names(&self, name: &str) -> Result<Vec<String>, SQLError> {
        let (_, table) = self.bound_table_for_security(name)?;
        let columns = table
            .columns
            .read()
            .iter()
            .map(|column| column.name.clone())
            .collect();
        Ok(columns)
    }

    pub(crate) fn ensure_column_privilege(
        &self,
        name: &str,
        column: &str,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        let current_user = self.current_user_name();
        self.ensure_column_privilege_for(name, column, &current_user, privilege)
    }

    pub(crate) fn ensure_column_privilege_for(
        &self,
        name: &str,
        column: &str,
        subject: &str,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        let (relation, table) = self.bound_table_for_security(name)?;
        let security = table.security();
        let roles = self.durable.roles.read();
        let memberships = self.durable.role_memberships.read();
        if column_privilege_check(
            &security,
            column,
            subject,
            TablePrivilegeCheck {
                privilege,
                grant_option: false,
            },
            &roles,
            &memberships,
        ) {
            return Ok(());
        }
        Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: format!("permission denied for table {}", relation.name),
        })
    }

    pub(crate) fn ensure_any_column_privilege(
        &self,
        name: &str,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        let current_user = self.current_user_name();
        self.ensure_any_column_privilege_for(name, &current_user, privilege)
    }

    pub(crate) fn ensure_any_column_privilege_for(
        &self,
        name: &str,
        subject: &str,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        let (relation, table) = self.bound_table_for_security(name)?;
        let security = table.security();
        let roles = self.durable.roles.read();
        let memberships = self.durable.role_memberships.read();
        let table_check = TablePrivilegeCheck {
            privilege,
            grant_option: false,
        };
        if role_has_privilege(&security, subject, table_check, &roles, &memberships)
            || table.columns.read().iter().any(|column| {
                column_privilege_check(
                    &security,
                    &column.name,
                    subject,
                    table_check,
                    &roles,
                    &memberships,
                )
            })
        {
            return Ok(());
        }
        Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: format!("permission denied for table {}", relation.name),
        })
    }

    pub(crate) fn ensure_view_privilege_for(
        &self,
        name: &str,
        view: &crate::StoredView,
        subject: &str,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        let relation = RelationIdentity::from_legacy_name(name).map_err(SQLError::Internal)?;
        let security = view.security();
        let roles = self.durable.roles.read();
        let memberships = self.durable.role_memberships.read();
        if role_has_privilege(
            &security,
            subject,
            TablePrivilegeCheck {
                privilege,
                grant_option: false,
            },
            &roles,
            &memberships,
        ) {
            return Ok(());
        }
        Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: format!(
                "permission denied for {} {}",
                match view.kind {
                    crate::StoredViewKind::View => "view",
                    crate::StoredViewKind::Materialized => "materialized view",
                },
                relation.name
            ),
        })
    }

    pub(crate) fn ensure_view_column_privilege_for(
        &self,
        name: &str,
        view: &crate::StoredView,
        column: &str,
        subject: &str,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        let relation = RelationIdentity::from_legacy_name(name).map_err(SQLError::Internal)?;
        let security = view.security();
        let roles = self.durable.roles.read();
        let memberships = self.durable.role_memberships.read();
        if column_privilege_check(
            &security,
            column,
            subject,
            TablePrivilegeCheck {
                privilege,
                grant_option: false,
            },
            &roles,
            &memberships,
        ) {
            return Ok(());
        }
        Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: format!(
                "permission denied for {} {}",
                match view.kind {
                    crate::StoredViewKind::View => "view",
                    crate::StoredViewKind::Materialized => "materialized view",
                },
                relation.name
            ),
        })
    }

    pub(crate) fn ensure_any_view_column_privilege_for(
        &self,
        name: &str,
        view: &crate::StoredView,
        subject: &str,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        let security = view.security();
        let roles = self.durable.roles.read();
        let memberships = self.durable.role_memberships.read();
        let check = TablePrivilegeCheck {
            privilege,
            grant_option: false,
        };
        let columns = view.output_columns.as_deref().ok_or_else(|| {
            SQLError::Internal(format!(
                "loaded view `{name}` has no durable public column metadata"
            ))
        })?;
        if role_has_privilege(&security, subject, check, &roles, &memberships)
            || columns.iter().any(|column| {
                column_privilege_check(&security, column, subject, check, &roles, &memberships)
            })
        {
            return Ok(());
        }
        drop(memberships);
        drop(roles);
        self.ensure_view_privilege_for(name, view, subject, privilege)
    }

    pub(crate) fn maintenance_table_names(&self, operation: &str) -> Result<Vec<String>, SQLError> {
        self.synchronize_table_catalog()
            .map_err(|error| SQLError::Internal(format!("load tables for {operation}: {error}")))?;
        let tables = self
            .storage
            .tables
            .read()
            .iter()
            .map(|(relation, table)| (relation.clone(), table.security()))
            .collect::<Vec<_>>();
        let current_user = self.current_user_name();
        let roles = self.durable.roles.read();
        let memberships = self.durable.role_memberships.read();
        let mut permitted = Vec::new();
        let mut denied = Vec::new();
        for (relation, security) in tables {
            if role_has_privilege(
                &security,
                &current_user,
                TablePrivilegeCheck {
                    privilege: TableAclPrivilege::Maintain,
                    grant_option: false,
                },
                &roles,
                &memberships,
            ) {
                permitted.push(relation.qualified_name());
            } else {
                denied.push(relation.name);
            }
        }
        drop(memberships);
        drop(roles);
        for name in denied {
            self.push_sql_notice(
                "WARNING",
                &format!("permission denied to {operation} \"{name}\", skipping it"),
            );
        }
        Ok(permitted)
    }

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
        let mut table_security = table.security();
        rewrite_acl_owner(&mut table_security, &new_owner);
        self.try_save_table_schema_with_components_and_security(
            name,
            &table,
            &columns,
            &constraints,
            &table_security,
        )
        .map_err(|error| SQLError::Internal(format!("persist table owner: {error}")))?;

        table.security.write().clone_from(&table_security);
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

    pub(crate) fn table_owned_sequence_owner_updates(
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
