//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native field decoding is shared by retained catalogs and synchronous image readers.

use super::super::fields::Privileges;
use super::{invalid, validate_identity, Fields};
use crate::{
    ast::ObjectAclEntry,
    catalog::roles::{identity::RoleBinding, RoleIdentity},
    SQLError,
};
use std::cmp::Ordering;
use uqa_core::{
    catalog_role::BoundAclEntry,
    memory::{Budgeted, BudgetedVec},
};

impl Fields<'_> {
    pub(super) async fn header(&mut self, magic: [u8; 8]) -> Result<usize, SQLError> {
        if self.binary.array::<8>().await? != magic {
            return Err(invalid("version header"));
        }
        Ok(self.binary.count().await?)
    }
    pub(super) async fn flag(&mut self) -> Result<bool, SQLError> {
        match self.binary.array::<1>().await?[0] {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(invalid("boolean tag")),
        }
    }
    pub(super) async fn oid(&mut self) -> Result<i64, SQLError> {
        let oid = i64::from_le_bytes(self.binary.array().await?);
        if oid <= 0 || oid > i64::from(u32::MAX) {
            return Err(invalid("object identifier"));
        }
        Ok(oid)
    }
    pub(super) async fn identity(&mut self) -> Result<RoleIdentity, SQLError> {
        let identity = RoleIdentity {
            oid: self.oid().await?,
            object_id: self.binary.array().await?,
        };
        validate_identity(identity)?;
        Ok(identity)
    }
    pub(super) async fn binding(&mut self) -> Result<Budgeted<RoleBinding>, SQLError> {
        let name = self.binary.text().await?;
        if name.is_empty() {
            return Err(invalid("empty role binding name"));
        }
        let oid = u32::from_le_bytes(self.binary.array().await?);
        let object_id = self.binary.array().await?;
        validate_identity(RoleIdentity {
            oid: i64::from(oid),
            object_id,
        })?;
        let (name, memory) = name.into_parts();
        Ok(Budgeted::new(
            RoleBinding::new(name, oid, object_id),
            memory,
        ))
    }
    pub(super) async fn compare_text(
        &mut self,
        left: &str,
        right: &str,
    ) -> Result<Ordering, SQLError> {
        let mut comparison = TextComparison::new(left, right);
        loop {
            self.binary.checkpoint().await?;
            if let Some(order) = comparison.advance() {
                return Ok(order);
            }
        }
    }
    pub(super) async fn ordered_name(
        &mut self,
        previous: Option<&String>,
    ) -> Result<Budgeted<String>, SQLError> {
        let name = self.binary.text().await?;
        if name.is_empty() {
            return Err(invalid("name order"));
        }
        if let Some(previous) = previous {
            if self.compare_text(previous, &name).await? != Ordering::Less {
                return Err(invalid("name order"));
            }
        }
        Ok(name)
    }
    pub(super) async fn acl<P: Privileges>(
        &mut self,
    ) -> Result<Budgeted<Option<Vec<BoundAclEntry<P>>>>, SQLError> {
        let memory = self.binary.memory;
        if !self.flag().await? {
            return Ok(Budgeted::new(None, memory.empty_reservation()));
        }
        let (entries, reservation) = self.acl_entries().await?.into_parts();
        Ok(Budgeted::new(Some(entries), reservation))
    }
    pub(super) async fn acl_entries<P: Privileges>(
        &mut self,
    ) -> Result<Budgeted<Vec<BoundAclEntry<P>>>, SQLError> {
        let memory = self.binary.memory;
        let count = self.binary.count().await?;
        let mut entries = BudgetedVec::new(memory);
        entries.reserve(count)?;
        for _ in 0..count {
            self.binary.cancellation.check()?;
            let role = if self.flag().await? {
                Some(self.identity().await?)
            } else {
                None
            };
            let grantor = self.identity().await?;
            let [privileges, grant_options] = self.binary.array().await?;
            entries.push(BoundAclEntry {
                role,
                grantor,
                privileges: P::from_bits(privileges).ok_or_else(|| invalid("privilege bits"))?,
                grant_options: P::from_bits(grant_options)
                    .ok_or_else(|| invalid("grant-option bits"))?,
            })?;
        }
        let (entries, memory) = entries.into_parts();
        Ok(Budgeted::new(entries, memory))
    }
    pub(super) async fn object_acl(&mut self) -> Result<Budgeted<Vec<ObjectAclEntry>>, SQLError> {
        let count = self.binary.count().await?;
        let mut entries = BudgetedVec::new(self.binary.memory);
        entries.reserve(count)?;
        for _ in 0..count {
            self.binary.cancellation.check()?;
            let role = if self.flag().await? {
                Some(self.identity().await?)
            } else {
                None
            };
            entries.push(ObjectAclEntry {
                role,
                grantor: self.identity().await?,
                grant_option: self.flag().await?,
            })?;
        }
        let (entries, reservation) = entries.into_parts();
        Ok(Budgeted::new(entries, reservation))
    }
}

/// One fixed comparison fragment, shared by synchronous and cooperative field readers.
pub(super) struct TextComparison<'a> {
    left: std::slice::Chunks<'a, u8>,
    right: std::slice::Chunks<'a, u8>,
    lengths: Ordering,
}
impl<'a> TextComparison<'a> {
    pub(super) fn new(left: &'a str, right: &'a str) -> Self {
        Self {
            left: left.as_bytes().chunks(4096),
            right: right.as_bytes().chunks(4096),
            lengths: left.len().cmp(&right.len()),
        }
    }
    pub(super) fn advance(&mut self) -> Option<Ordering> {
        match (self.left.next(), self.right.next()) {
            (Some(left), Some(right)) => {
                let order = left.cmp(right);
                (order != Ordering::Equal).then_some(order)
            }
            _ => Some(self.lengths),
        }
    }
}
