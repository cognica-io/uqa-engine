//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The privileges on the views a DML statement is rewritten through, which `PostgreSQL` checks once the rewrite is complete (`ExecCheckPermissions`), so that an error of the rewrite at any layer is reported before them.

use crate::catalog::roles::RoleReference;
use crate::semantics::view_privileges::{next_layer_privilege_subject, ViewPrivilegeCatalog};
use crate::SQLError;

/// The first privilege failure among the rewritten view layers, in rewrite order.
pub(super) struct LayerPrivileges {
    failure: Option<SQLError>,
}

impl LayerPrivileges {
    pub(super) const fn new() -> Self {
        Self { failure: None }
    }

    /// Run `check`, the privilege check of view `view` as `subject`, keeping its failure for [`Self::finish`] unless an earlier layer failed, and return the role the next layer is checked as.
    pub(super) fn check(
        &mut self,
        services: &dyn ViewPrivilegeCatalog,
        view: &str,
        subject: Option<&RoleReference>,
        check: impl FnOnce() -> Result<RoleReference, SQLError>,
    ) -> Result<RoleReference, SQLError> {
        match check() {
            Ok(next) => Ok(next),
            Err(error) => {
                self.failure.get_or_insert(error);
                next_layer_privilege_subject(services, view, subject)
            }
        }
    }

    /// Report the first failure, now that the rewrite has succeeded.
    pub(super) fn finish(self) -> Result<(), SQLError> {
        self.failure.map_or(Ok(()), Err)
    }
}
