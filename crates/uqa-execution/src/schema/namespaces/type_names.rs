//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reserve type destinations shared by domain declarations and relation row types.

use super::{relation_names::reserve_catalog_name, relations::RelationCreationContext};
use uqa_core::RelationIdentity;
use uqa_sql::{catalog::resolution::creation, SQLError};

pub const TYPE_CATALOG_CLASS_ID: u32 = 1247;

impl RelationCreationContext<'_> {
    /// Check visible types before a wait, then reject a newly committed competitor with catalog uniqueness diagnostics.
    pub fn reserve_type_name(&self, name: &str) -> Result<RelationIdentity, SQLError> {
        let identity = RelationIdentity::from_legacy_name(name).map_err(SQLError::Internal)?;
        creation::ensure_type_name_available(self.relations, &identity)?;
        self.reserve_type_destination(&identity)?;
        Ok(identity)
    }

    /// Row-bearing relations reserve both namespaces. Type preflight precedes either wait so concurrent collisions remain uniqueness violations.
    pub fn reserve_row_type_name(&self, name: &str) -> Result<(), SQLError> {
        let identity = RelationIdentity::from_legacy_name(name).map_err(SQLError::Internal)?;
        creation::ensure_type_name_available(self.relations, &identity)?;
        self.reserve_name(name)?;
        self.reserve_type_destination(&identity)
    }

    fn reserve_type_destination(&self, identity: &RelationIdentity) -> Result<(), SQLError> {
        reserve_catalog_name(
            self.locks,
            identity,
            TYPE_CATALOG_CLASS_ID,
            "pg_type_typname_nsp_index",
            || Ok(creation::type_name_in_use(self.relations, identity)),
        )
    }
}
