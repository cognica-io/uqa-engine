//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Declared column types affected by a domain constraint change.

use crate::catalog::domain::DomainCatalog;
use crate::expr::composites::{descriptor, CompositeTypeCatalog};
use crate::{ColumnType, SQLError};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DomainColumnDependency {
    None,
    /// The stored value has the domain's type or a domain derived from it.
    Direct,
    /// An array or composite lies between the stored column and the domain. `PostgreSQL` rejects scanning these containers when it validates a changed domain constraint.
    Container,
}

/// Classify a resolved column type using the statement's live domain and composite definitions. Embedded domain bases can predate catalog changes and are not a substitute for those definitions.
pub fn column_domain_dependency(
    ty: &ColumnType,
    target: u32,
    domains: &dyn DomainCatalog,
    composites: &dyn CompositeTypeCatalog,
) -> Result<DomainColumnDependency, SQLError> {
    Dependencies {
        target,
        domains,
        composites,
        visited: BTreeSet::new(),
    }
    .classify(ty)
}

struct Dependencies<'a> {
    target: u32,
    domains: &'a dyn DomainCatalog,
    composites: &'a dyn CompositeTypeCatalog,
    visited: BTreeSet<u32>,
}

impl Dependencies<'_> {
    fn classify(&mut self, ty: &ColumnType) -> Result<DomainColumnDependency, SQLError> {
        match ty {
            ColumnType::Domain { oid, .. } => {
                if *oid == self.target {
                    return Ok(DomainColumnDependency::Direct);
                }
                if !self.visited.insert(*oid) {
                    return Ok(DomainColumnDependency::None);
                }
                let domain = self.domains.domain_by_oid(*oid).ok_or_else(|| {
                    SQLError::Internal(format!(
                        "domain type OID {oid} is not available in the statement catalog"
                    ))
                })?;
                self.classify(&domain.definition.base)
            }
            ColumnType::Array(element) => self.container(element),
            ColumnType::Composite(reference) => {
                if !self.visited.insert(reference.oid) {
                    return Ok(DomainColumnDependency::None);
                }
                let descriptor = descriptor(Some(self.composites), reference.oid)?;
                for attribute in &descriptor.attributes {
                    if self.container(&attribute.ty)? == DomainColumnDependency::Container {
                        return Ok(DomainColumnDependency::Container);
                    }
                }
                Ok(DomainColumnDependency::None)
            }
            _ => Ok(DomainColumnDependency::None),
        }
    }

    fn container(&mut self, ty: &ColumnType) -> Result<DomainColumnDependency, SQLError> {
        self.classify(ty).map(|dependency| match dependency {
            DomainColumnDependency::None => DomainColumnDependency::None,
            DomainColumnDependency::Direct | DomainColumnDependency::Container => {
                DomainColumnDependency::Container
            }
        })
    }
}

#[cfg(test)]
mod tests;
