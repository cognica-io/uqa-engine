//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained read-only authorization keeps ACL contents independently of catalog mutation revisions.

use super::{BoundTableAclEntry, BoundTableSecurity, RoleIdentity, TableSecurity};
use crate::catalog::roles::RoleDefinitions;
use crate::{plan::copy::SyntaxProduction, SQLError};
use uqa_core::memory::{Produced, ProductionControl};

#[derive(Debug, PartialEq, Eq)]
pub struct CapturedTablePrivileges {
    pub role_owner: RoleIdentity,
    acl: Option<Vec<BoundTableAclEntry>>,
    column_acls: Vec<(String, Vec<BoundTableAclEntry>)>,
}

/// Full stored relation authority includes ACL tuple identities as well as the paths consulted by privilege checks. Ordered vectors avoid unaccounted native-map allocation in retained images.
#[derive(Debug, PartialEq, Eq)]
pub struct CapturedTableSecurity {
    pub privileges: CapturedTablePrivileges,
    pub relation_revision: Option<[u8; 16]>,
    pub column_revisions: Vec<(String, [u8; 16])>,
}

impl BoundTableSecurity {
    pub fn capture_security(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<CapturedTableSecurity>, SQLError> {
        let output = SyntaxProduction::new(*control);
        let privileges = output.take(self.capture_privileges(control)?);
        let mut revisions = output.vector(self.acl_revisions.columns.len())?;
        for (column, revision) in &self.acl_revisions.columns {
            revisions.push((output.text(column)?, *revision))?;
        }
        let captured = CapturedTableSecurity {
            privileges,
            relation_revision: self.acl_revisions.relation,
            column_revisions: revisions.finish()?,
        };
        output.finish(captured)
    }

    /// Retain every ACL path used by authorization, preserving absence and lexical column order.
    pub fn capture_privileges(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<CapturedTablePrivileges>, SQLError> {
        let output = SyntaxProduction::new(*control);
        let acl = self
            .acl
            .as_deref()
            .map(|acl| copy_acl(acl, &output))
            .transpose()?;
        let mut columns = output.vector(self.column_acls.len())?;
        for (name, entries) in &self.column_acls {
            let name = output.text(name)?;
            let entries = copy_acl(entries, &output)?;
            columns.push((name, entries))?;
        }
        let captured = CapturedTablePrivileges {
            role_owner: self.role_owner,
            acl,
            column_acls: columns.finish()?,
        };
        output.finish(captured)
    }
}

impl CapturedTablePrivileges {
    pub fn acl(&self) -> Option<&[BoundTableAclEntry]> {
        self.acl.as_deref()
    }

    pub fn column_acls(&self) -> &[(String, Vec<BoundTableAclEntry>)] {
        &self.column_acls
    }

    pub fn resolve(&self, roles: &RoleDefinitions) -> Result<TableSecurity, String> {
        super::resolve_privileges(
            self.role_owner,
            self.acl.as_deref(),
            self.column_acls.iter().map(|(name, acl)| (name, acl)),
            roles,
        )
    }
}

fn copy_acl(
    entries: &[BoundTableAclEntry],
    output: &SyntaxProduction<'_>,
) -> Result<Vec<BoundTableAclEntry>, SQLError> {
    let mut copied = output.vector(entries.len())?;
    for entry in entries {
        output.check()?;
        copied.push(entry.clone())?;
    }
    copied.finish()
}
