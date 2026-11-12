//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Admitted field decoding leaves native publication to the owning catalog.

use super::{fields::Privileges, invalid};
use crate::{ast::ObjectAclEntry, catalog::roles::RoleIdentity, SQLError};
use std::{cmp::Ordering, io::Read};
use uqa_core::{
    catalog_role::BoundAclEntry,
    image_io::{decode_image, AsyncImageInput, ImageInput},
    memory::{Budgeted, MemoryBudget},
    CancellationToken,
};

mod authorities;
mod callbacks;
mod collections;
mod column_types;
mod continuation;
mod domains;
mod expressions;
mod fields;
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
#[cfg(test)]
pub(super) mod window_tests;
pub(super) use authorities::{builtin_routines, system_relations};
pub use continuation::CatalogImageInput;
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

struct Fields<'a> {
    binary: AsyncImageInput<'a>,
    retained_children: &'a mut usize,
}

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
    fn fields<T, F>(&mut self, read: F) -> Result<T, SQLError>
    where
        F: for<'b> AsyncFnOnce(Fields<'b>) -> Result<T, SQLError>,
    {
        let retained_children = &mut self.retained_children;
        decode_image(
            self.binary.reader,
            self.binary.memory,
            self.binary.cancellation,
            async |binary| {
                read(Fields {
                    binary,
                    retained_children,
                })
                .await
            },
        )
    }
    fn header(&mut self, magic: [u8; 8]) -> Result<usize, SQLError> {
        self.fields(async |mut input| input.header(magic).await)
    }
    fn flag(&mut self) -> Result<bool, SQLError> {
        self.fields(async |mut input| input.flag().await)
    }
    fn oid(&mut self) -> Result<i64, SQLError> {
        self.fields(async |mut input| input.oid().await)
    }
    fn identity(&mut self) -> Result<RoleIdentity, SQLError> {
        self.fields(async |mut input| input.identity().await)
    }
    fn compare_text(&self, left: &str, right: &str) -> Result<Ordering, SQLError> {
        let mut comparison = fields::TextComparison::new(left, right);
        loop {
            self.binary.cancellation.check()?;
            if let Some(order) = comparison.advance() {
                return Ok(order);
            }
        }
    }
    fn ordered_name(&mut self, previous: Option<&String>) -> Result<Budgeted<String>, SQLError> {
        self.fields(async |mut input| input.ordered_name(previous).await)
    }
    fn acl<P: Privileges>(&mut self) -> Result<Budgeted<Option<Vec<BoundAclEntry<P>>>>, SQLError> {
        self.fields(async |mut input| input.acl::<P>().await)
    }
    fn acl_entries<P: Privileges>(&mut self) -> Result<Budgeted<Vec<BoundAclEntry<P>>>, SQLError> {
        self.fields(async |mut input| input.acl_entries::<P>().await)
    }
    fn object_acl(&mut self) -> Result<Budgeted<Vec<ObjectAclEntry>>, SQLError> {
        self.fields(async |mut input| input.object_acl().await)
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
