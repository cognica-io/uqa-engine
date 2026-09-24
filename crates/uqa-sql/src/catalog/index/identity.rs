//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validate stored index incarnations independently of their SQL names.

use super::IndexCatalogIdentity;
use crate::schema::constraint_metadata::{
    CatalogObjectAllocator, CatalogOidClass, ConstraintMetadataError, ConstraintMetadataResult,
};
use uqa_core::catalog_identity::CatalogObjectIdentity;

impl IndexCatalogIdentity {
    pub fn validate(&self, table_object_id: [u8; 16]) -> ConstraintMetadataResult<()> {
        if !self.identity.is_valid()
            || self.table_object_id == [0; 16]
            || self.table_object_id != table_object_id
            || self.physical_key.is_empty()
            || self.physical_key.contains('\0')
        {
            return Err(ConstraintMetadataError::Invalid(
                "invalid index catalog identity or indexed table incarnation".into(),
            ));
        }
        Ok(())
    }

    pub fn allocate(
        table_object_id: [u8; 16],
        allocate: &mut dyn CatalogObjectAllocator,
    ) -> ConstraintMetadataResult<Self> {
        let object_id = allocate.allocate_object_id("index")?;
        let oid = allocate.allocate_catalog_oid(CatalogOidClass::Relation, &object_id)?;
        let identity = Self {
            identity: CatalogObjectIdentity { object_id, oid },
            table_object_id,
            // A SQL-qualified identifier always contains a dot; this namespace cannot collide with a preserved legacy physical name.
            physical_key: format!("uqa:index:{:032x}", u128::from_be_bytes(object_id)),
        };
        identity.validate(table_object_id)?;
        Ok(identity)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_index_identity_rejects_invalid_addresses_tables_and_physical_keys() {
        let valid = IndexCatalogIdentity {
            identity: CatalogObjectIdentity {
                object_id: [1; 16],
                oid: 17000,
            },
            table_object_id: [2; 16],
            physical_key: "public.legacy_index".into(),
        };
        valid.validate([2; 16]).unwrap();
        assert!(valid.validate([3; 16]).is_err());
        for malformed in [
            IndexCatalogIdentity {
                table_object_id: [0; 16],
                ..valid.clone()
            },
            IndexCatalogIdentity {
                physical_key: String::new(),
                ..valid.clone()
            },
            IndexCatalogIdentity {
                physical_key: "invalid\0key".into(),
                ..valid.clone()
            },
            IndexCatalogIdentity {
                identity: CatalogObjectIdentity {
                    object_id: [0; 16],
                    ..valid.identity
                },
                ..valid.clone()
            },
            IndexCatalogIdentity {
                identity: CatalogObjectIdentity {
                    oid: 0,
                    ..valid.identity
                },
                ..valid.clone()
            },
            IndexCatalogIdentity {
                identity: CatalogObjectIdentity {
                    oid: i64::from(u32::MAX) + 1,
                    ..valid.identity
                },
                ..valid.clone()
            },
        ] {
            assert!(malformed.validate([2; 16]).is_err());
        }
    }
}
