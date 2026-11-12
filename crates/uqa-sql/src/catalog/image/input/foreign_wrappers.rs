//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Private wrapper images retain selected callbacks and declaration-order options.

use super::{invalid, metadata::Identities, Fields};
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

impl Fields<'_> {
    async fn wrapper_options(
        &mut self,
        count: usize,
    ) -> Result<Budgeted<Vec<(String, String)>>, SQLError> {
        let options = self.image_values::<(String, String)>(count).await?;
        let mut names = BudgetedMap::new(self.binary.memory);
        for (name, _) in options.iter() {
            self.binary.checkpoint().await?;
            if names.insert(name.as_str(), ())?.is_some() {
                return Err(invalid("duplicate foreign option"));
            }
        }
        Ok(options)
    }
    async fn foreign_wrapper(
        &mut self,
        key: &str,
        identities: &mut Identities,
    ) -> Result<Budgeted<ForeignWrapperDefinition>, SQLError> {
        let mut memory = self.binary.memory.empty_reservation();
        let (name, reservation) = self.binary.text().await?.into_parts();
        memory.absorb(reservation);
        if key.contains('\0') || self.compare_text(key, &name).await? != std::cmp::Ordering::Equal {
            return Err(invalid("foreign wrapper name"));
        }
        let identity = ForeignWrapperReference {
            oid: self.catalog_oid().await?,
            object_id: self.object_id().await?,
        };
        identities.oid(identity.oid)?;
        identities.object(identity.object_id)?;
        let owner = self.identity().await?;
        let count = self.binary.count().await?;
        let handler = match self.binary.array::<1>().await?[0] {
            0 => ForeignWrapperHandler::None,
            1 => ForeignWrapperHandler::Native(match self.binary.array::<1>().await?[0] {
                0 => NativeForeignWrapper::Memory,
                1 => NativeForeignWrapper::DuckDB,
                2 => NativeForeignWrapper::Arrow,
                _ => return Err(invalid("native foreign handler tag")),
            }),
            2 => {
                let (function, reservation) = self.foreign_callback().await?.into_parts();
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
        let validator = if self.flag().await? {
            let (value, reservation) = self.foreign_callback().await?.into_parts();
            memory.absorb(reservation);
            Some(value)
        } else {
            None
        };
        let (options, reservation) = self.wrapper_options(count).await?.into_parts();
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
    super::continuation::read_registry(reader, memory, cancellation)
}

impl ForeignWrappers {
    /// Retain native fields and claims across independent entry, byte and poll windows.
    pub fn image_input(
        memory: &MemoryBudget,
        mutations: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<super::CatalogImageInput<Self>, SQLError> {
        super::continuation::registry::<Self>(memory, mutations, cancellation)
    }
}

impl super::continuation::CatalogEntries for ForeignWrappers {
    type Scratch = Identities;
    const MAGIC: [u8; 8] = super::super::FOREIGN_WRAPPERS;
    fn create(memory: &MemoryBudget) -> (Self, Self::Scratch) {
        (Self::with_budget(memory), Identities::new(memory))
    }
    async fn read_entry(
        input: &mut Fields<'_>,
        result: &mut Self,
        identities: &mut Self::Scratch,
    ) -> Result<(), SQLError> {
        let key = input.ordered_name(result.last_durable_key()).await?;
        let value = input.foreign_wrapper(&key, identities).await?;
        result.insert_image_entry(key, value)?;

        Ok(())
    }
    fn finish(self, mutations: &MemoryBudget) -> Self {
        self.into_mutation_budget(mutations)
    }
}

impl super::collections::ImageValue for (String, String) {
    async fn read(input: &mut Fields<'_>) -> Result<Budgeted<Self>, SQLError> {
        let name = input.binary.text().await?;
        let value = input.binary.text().await?;
        if name.is_empty() || name.contains(['\0', '=']) {
            return Err(invalid("foreign option name"));
        }
        let (name, mut memory) = name.into_parts();
        let (value, reservation) = value.into_parts();
        memory.absorb(reservation);
        Ok(Budgeted::new((name, value), memory))
    }
}
