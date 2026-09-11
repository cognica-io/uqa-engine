//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::ast::{ColumnType, CreateDomain};
use serde::{Deserialize, Serialize};
use uqa_core::RelationIdentity;

pub fn domain_object_oid(object_id: &[u8; 16]) -> u32 {
    u32::try_from(super::oids::stable_object_oid("domain", object_id))
        .expect("catalog OIDs fit in u32")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredDomain {
    pub object_id: [u8; 16],
    pub oid: u32,
    pub identity: RelationIdentity,
    pub owner: String,
    pub definition: CreateDomain,
}

impl StoredDomain {
    pub fn column_type(&self) -> ColumnType {
        ColumnType::Domain {
            schema: self.identity.schema.clone(),
            name: self.identity.name.clone(),
            oid: self.oid,
            base: Box::new(self.definition.base.clone()),
        }
    }
}

/// Definition lookup for domain inheritance and constraint binding.
pub trait DomainCatalog {
    fn domain_by_oid(&self, oid: u32) -> Option<StoredDomain>;
}

pub fn domain_default_expression(
    catalog: &dyn DomainCatalog,
    ty: &crate::ColumnType,
) -> Option<crate::ast::Expr> {
    let crate::ColumnType::Domain { oid, base, .. } = ty else {
        return None;
    };
    catalog
        .domain_by_oid(*oid)
        .and_then(|domain| domain.definition.default)
        .or_else(|| domain_default_expression(catalog, base))
}
