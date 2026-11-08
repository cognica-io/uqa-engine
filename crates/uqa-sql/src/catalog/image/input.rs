//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Admitted field decoding leaves native publication to the owning catalog.

use super::{fields::Privileges, invalid};
use crate::{
    ast::ObjectAclEntry,
    catalog::roles::{identity::RoleBinding, RoleIdentity},
    SQLError,
};
use std::{cmp::Ordering, io::Read};
use uqa_core::{
    catalog_role::BoundAclEntry,
    image_io::ImageInput,
    memory::{Budgeted, BudgetedVec, MemoryBudget},
    CancellationToken,
};

mod authorities;
mod callbacks;
mod column_types;
mod domains;
mod expressions;
mod foreign_servers;
mod foreign_wrappers;
mod functions;
mod graph_oids;
mod memberships;
mod metadata;
mod plans;
mod roles;
mod scalars;
mod security;
mod sequences;
mod structured_types;
mod table_security;
mod tables;
mod triggers;
mod types;
mod views;
pub(super) use authorities::{builtin_routines, system_relations};
pub(super) use domains::domains;
pub(super) use expressions::expression_image;
pub(super) use foreign_servers::foreign_servers;
pub(super) use foreign_wrappers::foreign_wrappers;
pub(super) use functions::function_binding_image;
pub(super) use graph_oids::graph_oids;
pub(super) use memberships::memberships;
pub(super) use plans::{read_routines as routines, read_rules as rules};
pub use plans::{CapturedExpressionPlan, CapturedPlan, CapturedQuery, CapturedStatement};
pub(super) use roles::roles;
pub(super) use scalars::scalar_image;
pub use scalars::CapturedScalar;
pub(super) use security::{database, schemas};
pub(super) use sequences::sequence_authorities;
pub(super) use structured_types::{composites, ranges};
pub(super) use table_security::{bound_table_security, table_security};
pub use tables::{
    read_columns_image, read_columns_image_with_footprint, read_dropped_attributes_image,
    read_foreign_keys_image, read_table_checks_image, read_table_checks_image_with_footprint,
    read_table_hierarchy_image, read_table_hierarchy_image_with_footprint, read_table_keys_image,
};
pub(super) use triggers::triggers;
pub(super) use types::{enums, shells};
pub(super) use views::views;

struct Input<'a> {
    binary: ImageInput<'a>,
    retained_children: usize,
}
impl<'a> Input<'a> {
    fn new(
        reader: &'a mut dyn Read,
        memory: &'a MemoryBudget,
        cancellation: &'a CancellationToken,
    ) -> Self {
        Self {
            retained_children: 0,
            binary: ImageInput {
                reader,
                memory,
                cancellation,
            },
        }
    }
    fn header(&mut self, magic: [u8; 8]) -> Result<usize, SQLError> {
        if self.binary.array::<8>()? != magic {
            return Err(invalid("version header"));
        }
        Ok(self.binary.count()?)
    }
    fn flag(&mut self) -> Result<bool, SQLError> {
        match self.binary.array::<1>()?[0] {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(invalid("boolean tag")),
        }
    }
    fn oid(&mut self) -> Result<i64, SQLError> {
        let oid = i64::from_le_bytes(self.binary.array()?);
        if oid <= 0 || oid > i64::from(u32::MAX) {
            return Err(invalid("object identifier"));
        }
        Ok(oid)
    }
    fn identity(&mut self) -> Result<RoleIdentity, SQLError> {
        let identity = RoleIdentity {
            oid: self.oid()?,
            object_id: self.binary.array()?,
        };
        validate_identity(identity)?;
        Ok(identity)
    }
    fn binding(&mut self) -> Result<Budgeted<RoleBinding>, SQLError> {
        let name = self.binary.text()?;
        if name.is_empty() {
            return Err(invalid("empty role binding name"));
        }
        let oid = u32::from_le_bytes(self.binary.array()?);
        let object_id = self.binary.array()?;
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
    fn compare_text(&self, left: &str, right: &str) -> Result<Ordering, SQLError> {
        for (left, right) in left
            .as_bytes()
            .chunks(4096)
            .zip(right.as_bytes().chunks(4096))
        {
            self.binary.cancellation.check()?;
            let ordering = left.cmp(right);
            if ordering != Ordering::Equal {
                return Ok(ordering);
            }
        }
        self.binary.cancellation.check()?;
        Ok(left.len().cmp(&right.len()))
    }
    fn ordered_name(&mut self, previous: Option<&String>) -> Result<Budgeted<String>, SQLError> {
        let name = self.binary.text()?;
        if name.is_empty()
            || matches!(
                previous
                    .map(|key| self.compare_text(key, &name))
                    .transpose()?,
                Some(Ordering::Equal | Ordering::Greater)
            )
        {
            return Err(invalid("name order"));
        }
        Ok(name)
    }
    fn acl<P: Privileges>(&mut self) -> Result<Budgeted<Option<Vec<BoundAclEntry<P>>>>, SQLError> {
        let memory = self.binary.memory;
        if !self.flag()? {
            return Ok(Budgeted::new(None, memory.empty_reservation()));
        }
        let (entries, reservation) = self.acl_entries()?.into_parts();
        Ok(Budgeted::new(Some(entries), reservation))
    }
    fn acl_entries<P: Privileges>(&mut self) -> Result<Budgeted<Vec<BoundAclEntry<P>>>, SQLError> {
        let memory = self.binary.memory;
        let count = self.binary.count()?;
        let mut entries = BudgetedVec::new(memory);
        entries.reserve(count)?;
        for _ in 0..count {
            self.binary.cancellation.check()?;
            let role = if self.flag()? {
                Some(self.identity()?)
            } else {
                None
            };
            let grantor = self.identity()?;
            let [privileges, grant_options] = self.binary.array()?;
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
    fn object_acl(&mut self) -> Result<Budgeted<Vec<ObjectAclEntry>>, SQLError> {
        let count = self.binary.count()?;
        let mut entries = BudgetedVec::new(self.binary.memory);
        entries.reserve(count)?;
        for _ in 0..count {
            self.binary.cancellation.check()?;
            let role = if self.flag()? {
                Some(self.identity()?)
            } else {
                None
            };
            entries.push(ObjectAclEntry {
                role,
                grantor: self.identity()?,
                grant_option: self.flag()?,
            })?;
        }
        let (entries, reservation) = entries.into_parts();
        Ok(Budgeted::new(entries, reservation))
    }
}

fn validate_identity(identity: RoleIdentity) -> Result<(), SQLError> {
    if !identity.is_valid()
        || ((identity.oid == RoleIdentity::BOOTSTRAP.oid)
            != (identity.object_id == RoleIdentity::BOOTSTRAP.object_id))
    {
        return Err(invalid("role identity"));
    }
    Ok(())
}
