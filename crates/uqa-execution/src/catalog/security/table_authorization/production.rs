//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Table authorization borrows immutable ACL generations and admits only selected lookup names.

use super::{RoleSubject, TableAclPrivilege, TableAuthorizationContext, TablePrivilegeState};
use std::sync::Arc;
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    RelationIdentity, RelationName,
};
use uqa_sql::{catalog::security::table::selection::RelationPrivileges, SQLError};

impl TableAuthorizationContext<'_> {
    pub fn bound_table_for_security_with_control(
        &self,
        name: &str,
        control: &ProductionControl<'_>,
    ) -> Result<(Produced<RelationIdentity>, Arc<dyn TablePrivilegeState>), SQLError> {
        control.check()?;
        let reference = RelationIdentity::try_borrowed_reference(name).map_err(|error| {
            match control.format(format_args!("resolve table `{name}`: {error}")) {
                Ok(message) => SQLError::Internal(message.into_parts().0),
                Err(error) => error.into(),
            }
        })?;
        let relation = reference.identity_with_control(RelationName::decoded("public"), control)?;
        let table = self
            .registry
            .retained_table_with_control(&relation, control)?
            .ok_or_else(
                || match control.format(format_args!("table `{name}` disappeared")) {
                    Ok(message) => SQLError::Internal(message.into_parts().0),
                    Err(error) => error.into(),
                },
            )?;
        Ok((relation, table))
    }

    pub fn bound_table_column_names_with_control(
        &self,
        name: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Vec<String>>, SQLError> {
        let mut names = ProductionVec::new(*control);
        self.visit_bound_table_column_names_with_control(name, control, &mut |name| {
            names.push_produced(control.copy_text(name)?)?;
            Ok(())
        })?;
        Ok(names.finish()?)
    }

    pub fn visit_bound_table_column_names_with_control(
        &self,
        name: &str,
        control: &ProductionControl<'_>,
        read: &mut dyn FnMut(&str) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        let (_, table) = self.bound_table_for_security_with_control(name, control)?;
        table.with_columns_with_control(control, &mut |columns| {
            for column in columns {
                control.check()?;
                read(&column.name)?;
            }
            Ok(())
        })
    }

    pub fn ensure_table_privilege_for_with_control(
        &self,
        name: &str,
        subject: &(impl RoleSubject + ?Sized),
        privilege: TableAclPrivilege,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        self.require(name, None, subject, privilege, control)
    }

    pub fn ensure_column_privilege_for_with_control(
        &self,
        name: &str,
        column: &str,
        subject: &(impl RoleSubject + ?Sized),
        privilege: TableAclPrivilege,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        self.require(name, Some(column), subject, privilege, control)
    }

    fn require(
        &self,
        name: &str,
        column: Option<&str>,
        subject: &(impl RoleSubject + ?Sized),
        privilege: TableAclPrivilege,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        let (relation, table) = self.bound_table_for_security_with_control(name, control)?;
        let security = table.security_snapshot_with_control(control)?;
        let mut allowed = false;
        self.roles
            .with_role_catalogs_with_control(control, &mut |roles, memberships| {
                allowed = RelationPrivileges {
                    subject,
                    roles,
                    memberships,
                    control: *control,
                }
                .relation_for(&security, column, privilege)?;
                Ok(())
            })?;
        if allowed {
            Ok(())
        } else {
            Err(denied(&relation.name, control))
        }
    }

    pub fn ensure_any_column_privilege_for_with_control(
        &self,
        name: &str,
        subject: &(impl RoleSubject + ?Sized),
        privilege: TableAclPrivilege,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        let (relation, table) = self.bound_table_for_security_with_control(name, control)?;
        let security = table.security_snapshot_with_control(control)?;
        let mut allowed = false;
        self.roles
            .with_role_catalogs_with_control(control, &mut |roles, memberships| {
                let access = RelationPrivileges {
                    subject,
                    roles,
                    memberships,
                    control: *control,
                };
                allowed = access.any_column_for(&security, privilege, |test| {
                    let mut found = false;
                    table.with_columns_with_control(control, &mut |columns| {
                        for column in columns {
                            if test(&column.name)? {
                                found = true;
                                break;
                            }
                        }
                        Ok(())
                    })?;
                    Ok(found)
                })?;
                Ok(())
            })?;
        if allowed {
            Ok(())
        } else {
            Err(denied(&relation.name, control))
        }
    }
}

fn denied(name: &str, control: &ProductionControl<'_>) -> SQLError {
    match control.format(format_args!("permission denied for table {name}")) {
        Ok(message) => SQLError::Routine {
            sqlstate: "42501".into(),
            message: message.into_parts().0,
        },
        Err(error) => error.into(),
    }
}
