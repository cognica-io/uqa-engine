//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained read-only authorization keeps ACL contents independently of catalog mutation revisions.

use super::{BoundTableAclEntry, BoundTableSecurity, RoleDefinition, RoleIdentity, TableSecurity};
use crate::{plan::copy::SyntaxProduction, SQLError};
use std::collections::BTreeMap;
use uqa_core::memory::{Produced, ProductionControl};

#[derive(Debug)]
pub struct CapturedTablePrivileges {
    pub role_owner: RoleIdentity,
    acl: Option<Vec<BoundTableAclEntry>>,
    column_acls: Vec<(String, Vec<BoundTableAclEntry>)>,
}

impl BoundTableSecurity {
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
    pub fn resolve(
        &self,
        roles: &BTreeMap<String, RoleDefinition>,
    ) -> Result<TableSecurity, String> {
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
