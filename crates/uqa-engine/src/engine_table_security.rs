//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable ordinary-table ownership, access-control lists, and authorization checks.

use std::collections::BTreeMap;
use std::sync::Arc;

use uqa_sql::ast::{
    GrantSequenceStmt, GrantSequenceTarget, GrantTableStmt, GrantTableTarget, SequencePrivilege,
    SequenceRevokeBehavior, TablePrivilege, TableRevokeBehavior,
};
use uqa_sql::SQLError;

mod acl;
mod columns;
mod inquiry;

pub(crate) use acl::TableAclPrivilege;
use acl::{
    grant_acl, requested_acl_privileges, revoke_acl, rewrite_acl_owner, role_has_privilege,
    select_acl_grantor, RequestedTablePrivileges, TablePrivilegeCheck,
};
use columns::{
    grant_column_acl, revoke_column_acl, role_has_column_privilege as column_privilege_check,
    select_column_acl_grantor,
};

use crate::engine_capabilities::RelationResolution;
use crate::engine_roles::{role_can_set, RoleDefinition, RoleMembership, RoleMembershipKey};
use crate::engine_schema_security::SchemaAclPrivilege;
use crate::engine_state::{SequenceSecurity, TableSecurity};
use crate::{Engine, RelationIdentity, TableState};

struct ResolvedTableGrantTarget {
    requested: String,
    name: String,
    relation: RelationIdentity,
    kind: &'static str,
}

enum ResolvedTablePrivilegeTarget {
    Table(RelationIdentity),
    Sequence(RelationIdentity),
}

enum ResolvedColumnPrivilegeTarget {
    User(String),
    System,
}

const POSTGRES_SYSTEM_COLUMNS: [&str; 6] = ["ctid", "xmin", "cmin", "xmax", "cmax", "tableoid"];

fn apply_table_acl(
    statement: &GrantTableStmt,
    grantees: &[String],
    privileges: &[TableAclPrivilege],
    current_user: &str,
    roles: &BTreeMap<String, RoleDefinition>,
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
    current: &TableSecurity,
) -> Result<(TableSecurity, usize), SQLError> {
    let grantors = privileges
        .iter()
        .map(|privilege| {
            (
                *privilege,
                select_acl_grantor(current, *privilege, current_user, roles, memberships),
            )
        })
        .collect::<Vec<_>>();
    let grantable = grantors
        .iter()
        .filter(|(_, grantor)| grantor.is_some())
        .count();
    let mut next = current.clone();
    for (privilege, grantor) in grantors {
        let Some(grantor) = grantor else {
            continue;
        };
        if statement.is_grant {
            grant_acl(
                &mut next,
                privilege,
                grantees,
                &grantor,
                statement.grant_option,
            );
        } else {
            revoke_acl(
                &mut next,
                privilege,
                grantees,
                &grantor,
                statement.grant_option_only,
                statement.revoke_behavior == TableRevokeBehavior::Cascade,
            )?;
        }
    }
    Ok((next, grantable))
}

fn apply_column_acl(
    statement: &GrantTableStmt,
    grantees: &[String],
    privileges: &[(TableAclPrivilege, String)],
    current_user: &str,
    roles: &BTreeMap<String, RoleDefinition>,
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
    current: &TableSecurity,
) -> Result<(TableSecurity, usize), SQLError> {
    let grantors = privileges
        .iter()
        .map(|(privilege, column)| {
            (
                *privilege,
                column.clone(),
                select_column_acl_grantor(
                    current,
                    column,
                    *privilege,
                    current_user,
                    roles,
                    memberships,
                ),
            )
        })
        .collect::<Vec<_>>();
    let grantable = grantors
        .iter()
        .filter(|(_, _, grantor)| grantor.is_some())
        .count();
    let mut next = current.clone();
    for (privilege, column, grantor) in grantors {
        let Some(grantor) = grantor else {
            continue;
        };
        if statement.is_grant {
            grant_column_acl(
                &mut next,
                &column,
                privilege,
                grantees,
                &grantor,
                statement.grant_option,
            );
        } else {
            revoke_column_acl(
                &mut next,
                &column,
                privilege,
                grantees,
                &grantor,
                statement.grant_option_only,
                statement.revoke_behavior == TableRevokeBehavior::Cascade,
            )?;
        }
    }
    Ok((next, grantable))
}

fn validate_requested_columns(
    target: &RelationIdentity,
    columns: &[uqa_sql::ast::ColumnDef],
    requested: &RequestedTablePrivileges,
) -> Result<(), SQLError> {
    for (_, requested_column) in &requested.columns {
        if !columns
            .iter()
            .any(|column| column.name == *requested_column)
        {
            return Err(SQLError::Routine {
                sqlstate: "42703".into(),
                message: format!(
                    "column \"{requested_column}\" of relation \"{}\" does not exist",
                    target.name
                ),
            });
        }
    }
    Ok(())
}

fn validate_table_grant_target_kinds(
    statement: &GrantTableStmt,
    targets: &[ResolvedTableGrantTarget],
) -> Result<(), SQLError> {
    for target in targets {
        if !matches!(target.kind, "table" | "sequence") {
            return Err(SQLError::Unsupported(format!(
                "{} privileges for \"{}\" are not supported",
                target.kind, target.requested
            )));
        }
    }
    if let Some(column) = statement
        .privileges
        .iter()
        .flat_map(|privilege| &privilege.columns)
        .next()
    {
        if let Some(target) = targets.iter().find(|target| target.kind == "sequence") {
            return Err(SQLError::Routine {
                sqlstate: "42703".into(),
                message: format!(
                    "column \"{column}\" of relation \"{}\" does not exist",
                    target.relation.name
                ),
            });
        }
    }
    Ok(())
}

fn validate_table_acl_roles(
    statement: &GrantTableStmt,
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
    if statement.is_grant && statement.grant_option && grantees.iter().any(|role| role == "PUBLIC")
    {
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

fn table_sequence_privileges(
    privileges: &[uqa_sql::ast::TablePrivilegeSpec],
) -> (Vec<SequencePrivilege>, bool) {
    if privileges.is_empty() {
        return (
            vec![
                SequencePrivilege::Select,
                SequencePrivilege::Update,
                SequencePrivilege::Usage,
            ],
            false,
        );
    }
    let mut mapped = Vec::new();
    let mut inapplicable = false;
    for spec in privileges {
        let privilege = if spec.columns.is_empty() {
            match &spec.privilege {
                TablePrivilege::Select => Some(SequencePrivilege::Select),
                TablePrivilege::Update => Some(SequencePrivilege::Update),
                TablePrivilege::Usage => Some(SequencePrivilege::Usage),
                _ => {
                    inapplicable = true;
                    None
                }
            }
        } else {
            Some(SequencePrivilege::ColumnsUnsupported)
        };
        if let Some(privilege) = privilege {
            if !mapped.contains(&privilege) {
                mapped.push(privilege);
            }
        }
    }
    (mapped, inapplicable)
}

fn table_acl_warning(is_grant: bool, partial: bool, name: &str) -> (&'static str, String) {
    let message = match (is_grant, partial) {
        (true, true) => format!("not all privileges were granted for \"{name}\""),
        (true, false) => format!("no privileges were granted for \"{name}\""),
        (false, true) => format!("not all privileges could be revoked for \"{name}\""),
        (false, false) => format!("no privileges could be revoked for \"{name}\""),
    };
    ("WARNING", message)
}

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
        let has_tables = targets.iter().any(|target| target.kind == "table");
        let requested_privileges = if has_tables
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
        let table_targets = self.validated_table_grant_targets(&targets, &requested_privileges)?;
        let mut updates = Vec::new();
        let mut notices = Vec::new();
        for (target, table) in table_targets {
            let current = table.security();
            let (next, table_grantable) = apply_table_acl(
                statement,
                &grantees,
                &requested_privileges.table,
                &current_user,
                &roles,
                &memberships,
                &current,
            )?;
            let (next, column_grantable) = apply_column_acl(
                statement,
                &grantees,
                &requested_privileges.columns,
                &current_user,
                &roles,
                &memberships,
                &next,
            )?;
            let requested_count =
                requested_privileges.table.len() + requested_privileges.columns.len();
            let grantable = table_grantable + column_grantable;
            if grantable != requested_count {
                notices.push(table_acl_warning(
                    statement.is_grant,
                    grantable != 0,
                    &target.relation.name,
                ));
            }
            if next != current {
                updates.push((target.name.clone(), table, next));
            }
        }
        for (name, table, security) in &updates {
            self.persist_table_security(name, table, security)?;
        }
        let table_changed = !updates.is_empty();
        for (_, table, security) in updates {
            table.security.write().clone_from(&security);
        }
        drop(memberships);
        drop(roles);

        self.grant_table_syntax_sequence_privileges(statement, &targets)?;
        for (level, message) in notices {
            self.push_sql_notice(level, &message);
        }
        if table_changed {
            self.note_table_catalog_changed();
        }
        Ok(())
    }

    fn validated_table_grant_targets<'a>(
        &self,
        targets: &'a [ResolvedTableGrantTarget],
        requested: &RequestedTablePrivileges,
    ) -> Result<Vec<(&'a ResolvedTableGrantTarget, Arc<TableState>)>, SQLError> {
        let tables = self.storage.tables.read();
        targets
            .iter()
            .filter(|target| target.kind == "table")
            .map(|target| {
                let table = tables.get(&target.relation).cloned().ok_or_else(|| {
                    SQLError::Internal(format!("table `{}` disappeared", target.name))
                })?;
                validate_requested_columns(&target.relation, &table.columns.read(), requested)?;
                Ok((target, table))
            })
            .collect()
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
        let (relation, table) = self.bound_table_for_security(name)?;
        let security = table.security();
        let current_user = self.current_user_name();
        let roles = self.durable.roles.read();
        let memberships = self.durable.role_memberships.read();
        if role_has_privilege(
            &security,
            &current_user,
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
        let (relation, table) = self.bound_table_for_security(name)?;
        let security = table.security();
        let current_user = self.current_user_name();
        let roles = self.durable.roles.read();
        let memberships = self.durable.role_memberships.read();
        if column_privilege_check(
            &security,
            column,
            &current_user,
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
        let (relation, table) = self.bound_table_for_security(name)?;
        let security = table.security();
        let current_user = self.current_user_name();
        let roles = self.durable.roles.read();
        let memberships = self.durable.role_memberships.read();
        let table_check = TablePrivilegeCheck {
            privilege,
            grant_option: false,
        };
        if role_has_privilege(&security, &current_user, table_check, &roles, &memberships)
            || table.columns.read().iter().any(|column| {
                column_privilege_check(
                    &security,
                    &column.name,
                    &current_user,
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
