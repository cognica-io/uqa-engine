//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Private wrapper images retain selected callbacks and declaration-order options.

use super::{invalid, metadata::Identities, Input};
use crate::{
    catalog::foreign_wrapper::{
        ForeignWrapperDefinition, ForeignWrapperHandler, ForeignWrapperReference, ForeignWrappers,
        NativeForeignWrapper,
    },
    SQLError,
};
use std::io::Read;
use uqa_core::{
    memory::{Budgeted, BudgetedMap, MemoryBudget},
    CancellationToken,
};

impl Input<'_> {
    fn wrapper_options(
        &mut self,
        count: usize,
    ) -> Result<Budgeted<Vec<(String, String)>>, SQLError> {
        let options = self.image_values(count, |input| {
            let (name, mut memory) = input.binary.text()?.into_parts();
            let (value, reservation) = input.binary.text()?.into_parts();
            memory.absorb(reservation);
            if name.is_empty() || name.contains(['\0', '=']) {
                return Err(invalid("foreign option name"));
            }
            Ok(Budgeted::new((name, value), memory))
        })?;
        let mut names = BudgetedMap::new(self.binary.memory);
        for (name, _) in options.iter() {
            self.binary.cancellation.check()?;
            if names.insert(name.as_str(), ())?.is_some() {
                return Err(invalid("duplicate foreign option"));
            }
        }
        Ok(options)
    }
    fn foreign_wrapper(
        &mut self,
        key: &str,
        identities: &mut Identities,
    ) -> Result<Budgeted<ForeignWrapperDefinition>, SQLError> {
        let (name, mut memory) = self.binary.text()?.into_parts();
        if key.contains('\0') || self.compare_text(key, &name)? != std::cmp::Ordering::Equal {
            return Err(invalid("foreign wrapper name"));
        }
        let identity = ForeignWrapperReference {
            oid: self.catalog_oid()?,
            object_id: self.object_id()?,
        };
        identities.oid(identity.oid)?;
        identities.object(identity.object_id)?;
        let owner = self.identity()?;
        let count = self.binary.count()?;
        let handler = match self.binary.array::<1>()?[0] {
            0 => ForeignWrapperHandler::None,
            1 => ForeignWrapperHandler::Native(match self.binary.array::<1>()?[0] {
                0 => NativeForeignWrapper::Memory,
                1 => NativeForeignWrapper::DuckDB,
                2 => NativeForeignWrapper::Arrow,
                _ => return Err(invalid("native foreign handler tag")),
            }),
            2 => {
                let (function, reservation) = self.foreign_callback()?.into_parts();
                memory.absorb(reservation);
                ForeignWrapperHandler::Function(function)
            }
            _ => return Err(invalid("foreign handler tag")),
        };
        if identity.oid < crate::catalog::oids::FIRST_NORMAL_OBJECT_ID
            && !matches!(handler,ForeignWrapperHandler::Native(native) if identity==native.reference())
        {
            return Err(invalid("reserved foreign wrapper identity"));
        }
        let validator = if self.flag()? {
            let (value, reservation) = self.foreign_callback()?.into_parts();
            memory.absorb(reservation);
            Some(value)
        } else {
            None
        };
        let (options, reservation) = self.wrapper_options(count)?.into_parts();
        memory.absorb(reservation);
        Ok(Budgeted::new(
            ForeignWrapperDefinition {
                name,
                identity,
                owner,
                handler,
                validator,
                options,
            },
            memory,
        ))
    }
}

pub(in super::super) fn foreign_wrappers(
    reader: &mut dyn Read,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<ForeignWrappers, SQLError> {
    let mut input = Input::new(reader, memory, cancellation);
    let count = input.header(super::super::FOREIGN_WRAPPERS)?;
    let mut result = ForeignWrappers::with_budget(memory);
    let mut identities = Identities::new(memory);
    for _ in 0..count {
        cancellation.check()?;
        let key = input.ordered_name(result.last_durable_key())?;
        let value = input.foreign_wrapper(&key, &mut identities)?;
        result.insert_image_entry(key, value)?;
    }
    input.binary.end()?;
    Ok(result)
}
