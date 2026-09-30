//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The OIDs of a session's temporary namespace, drawn from the database's counter when the session's first temporary object creates it, as `InitTempTableNamespace` creates `pg_temp_N` and then `pg_toast_temp_N` before the object takes its own OIDs.

use uqa_sql::catalog::temporary_namespace::TemporaryNamespaceOids;
use uqa_sql::SQLError;

use super::ReservedCatalogIdentityAllocator;

impl ReservedCatalogIdentityAllocator<'_> {
    /// The temporary namespace's OID and then its TOAST namespace's; `namespace_in_use` tells whether a namespace already holds an OID.
    pub fn allocate_temporary_namespace_oids(
        &mut self,
        mut namespace_in_use: impl FnMut(i64) -> Result<bool, SQLError>,
    ) -> Result<TemporaryNamespaceOids, SQLError> {
        let namespace = self.allocate_namespace_oid(&mut namespace_in_use)?;
        let toast_namespace = self.allocate_namespace_oid(|oid| {
            Ok(oid == i64::from(namespace) || namespace_in_use(oid)?)
        })?;
        Ok(TemporaryNamespaceOids {
            namespace,
            toast_namespace,
        })
    }
}
