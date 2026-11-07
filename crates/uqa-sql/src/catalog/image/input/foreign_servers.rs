//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Foreign server declarations retain exact option values and their independently stored order.

use super::{invalid, metadata::Identities, Input};
use crate::{
    catalog::{
        foreign_server::{ForeignServerDefinition, ForeignServerMetadata, ForeignServers},
        foreign_wrapper::ForeignWrapperReference,
    },
    SQLError,
};
use std::{io::Read, ops::Bound};
use uqa_core::{
    memory::{Budgeted, BudgetedMap, BudgetedVec, MemoryBudget, OwnedMap},
    CancellationToken,
};

impl Input<'_> {
    fn server_options(
        &mut self,
        count: usize,
    ) -> Result<Budgeted<OwnedMap<String, String>>, SQLError> {
        let mut reservation = self.binary.memory.empty_reservation();
        let mut options = OwnedMap::<String, String>::new();
        for _ in 0..count {
            self.binary.cancellation.check()?;
            let key = self.binary.text()?;
            if options.first_from(Bound::Included(&*key)).is_some() {
                return Err(invalid("foreign option order"));
            }
            let value = self.binary.text()?;
            reservation.grow(OwnedMap::<String, String>::entry_bytes())?;
            let (key, key_memory) = key.into_parts();
            let (value, value_memory) = value.into_parts();
            reservation.absorb(key_memory);
            reservation.absorb(value_memory);
            options.insert(key, value);
        }
        Ok(Budgeted::new(options, reservation))
    }

    fn server_order(
        &mut self,
        count: Option<usize>,
        options: &OwnedMap<String, String>,
    ) -> Result<Budgeted<Option<Vec<String>>>, SQLError> {
        let mut reservation = self.binary.memory.empty_reservation();
        let Some(count) = count else {
            return Ok(Budgeted::new(None, reservation));
        };
        if count != options.len() {
            return Err(invalid("foreign option-order count"));
        }
        let mut order = BudgetedVec::new(self.binary.memory);
        order.reserve(count)?;
        for _ in 0..count {
            let name = self.binary.text()?;
            if !options.contains_key(&*name) {
                return Err(invalid("unknown foreign option"));
            }
            let (name, name_memory) = name.into_parts();
            reservation.absorb(name_memory);
            order.push(name)?;
        }
        let mut seen = BudgetedMap::new(self.binary.memory);
        for name in order.iter() {
            self.binary.cancellation.check()?;
            if seen.insert(name.as_str(), ())?.is_some() {
                return Err(invalid("duplicate foreign option"));
            }
        }
        drop(seen);
        let (order, order_memory) = order.into_parts();
        reservation.absorb(order_memory);
        Ok(Budgeted::new(Some(order), reservation))
    }

    fn foreign_server(
        &mut self,
        key: &str,
        identities: &mut Identities,
    ) -> Result<Budgeted<ForeignServerDefinition>, SQLError> {
        let mut reservation = self.binary.memory.empty_reservation();
        let (name, name_memory) = self.binary.text()?.into_parts();
        reservation.absorb(name_memory);
        if self.compare_text(key, &name)? != std::cmp::Ordering::Equal {
            return Err(invalid("foreign server name"));
        }
        let (fdw_type, type_memory) = self.binary.text()?.into_parts();
        reservation.absorb(type_memory);
        if fdw_type.is_empty() {
            return Err(invalid("foreign wrapper name"));
        }
        let oid = self.user_oid()?;
        identities.oid(oid)?;
        let object_id = self.object_id()?;
        identities.object(object_id)?;
        let owner = self.identity()?;
        let (server_type, type_memory) = self.optional_text()?.into_parts();
        reservation.absorb(type_memory);
        let (version, version_memory) = self.optional_text()?.into_parts();
        reservation.absorb(version_memory);
        let wrapper_reference = if self.flag()? {
            Some(ForeignWrapperReference {
                oid: self.catalog_oid()?,
                object_id: self.object_id()?,
            })
        } else {
            None
        };
        let count = self.binary.count()?;
        let order_count = if self.flag()? {
            Some(self.binary.count()?)
        } else {
            None
        };
        let (options, options_memory) = self.server_options(count)?.into_parts();
        reservation.absorb(options_memory);
        let (option_order, order_memory) = self.server_order(order_count, &options)?.into_parts();
        reservation.absorb(order_memory);
        Ok(Budgeted::new(
            ForeignServerDefinition {
                name,
                fdw_type,
                options,
                metadata: ForeignServerMetadata {
                    oid,
                    object_id,
                    owner,
                    server_type,
                    version,
                    option_order,
                    wrapper_reference,
                },
            },
            reservation,
        ))
    }
}

pub(in super::super) fn foreign_servers(
    reader: &mut dyn Read,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<ForeignServers, SQLError> {
    let mut input = Input::new(reader, memory, cancellation);
    let count = input.header(super::super::FOREIGN_SERVERS)?;
    let mut result = ForeignServers::with_budget(memory);
    let mut identities = Identities::new(memory);
    for _ in 0..count {
        cancellation.check()?;
        let key = input.ordered_name(result.last_durable_key())?;
        let value = input.foreign_server(&key, &mut identities)?;
        result.insert_image_entry(key, value)?;
    }
    input.binary.end()?;
    Ok(result)
}
