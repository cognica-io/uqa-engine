//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relation privileges use the shared ordered grant closure without changing their carrier.

use super::{acl_grantor, TableAclEntry, TableAclPrivilege};
use crate::{
    catalog::{
        security::grant_paths::{self, Grant, Paths},
        work::{inline, Work},
    },
    SQLError,
};
use uqa_core::memory::ProductionControl;

impl Grant for TableAclEntry {
    type Privilege = TableAclPrivilege;
    fn recipient(&self) -> Option<&str> {
        self.role.role_name()
    }
    fn grantor<'a>(&'a self, owner: &'a str) -> &'a str {
        acl_grantor(self, owner)
    }
    fn grants_option(&self, privilege: Self::Privilege) -> bool {
        self.grant_options.intersects(privilege.mask())
    }
}

pub(in crate::catalog::security) fn extend<'a>(
    owner: &'a str,
    acl: Option<&'a [TableAclEntry]>,
    privilege: TableAclPrivilege,
    reachable: &mut impl Paths<'a>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    inline(extend_from(
        owner,
        acl,
        privilege,
        reachable,
        control,
        &mut (),
    ))
}

pub(in crate::catalog::security) async fn extend_from<'a>(
    owner: &'a str,
    acl: Option<&'a [TableAclEntry]>,
    privilege: TableAclPrivilege,
    reachable: &mut impl Paths<'a>,
    control: &ProductionControl<'_>,
    work: &mut impl Work,
) -> Result<(), SQLError> {
    grant_paths::extend_from(owner, acl, privilege, reachable, control, work).await
}
