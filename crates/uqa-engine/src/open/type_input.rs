//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The private session retains complete enum and shell generations in ordinary restoration order.

use super::Engine;
use std::sync::Arc;
use uqa_execution::catalog::{enum_type, shell_type};
use uqa_storage::{
    catalog::input::CatalogInputLimits, read_control::StorageReadControl, StorageBackendResult,
};

pub(super) enum PersistentTypeInput {
    Enums(enum_type::EnumCatalogInput),
    Shells(shell_type::ShellCatalogInput),
}

impl PersistentTypeInput {
    pub(super) fn new(
        engine: &Engine,
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        Ok(Self::Enums(enum_type::begin_input(
            Arc::clone(engine.storage.catalog.as_ref().expect("persistent catalog")),
            engine.durable.roles.read().clone(),
            limits,
            engine.durable.enums.read().budget(),
            control,
        )?))
    }

    /// Every failure consumes its unfinished input; the enclosing candidate owns the complete private session and transaction.
    pub(super) fn advance(
        self,
        engine: &Engine,
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<Self>> {
        match self {
            Self::Enums(mut input) => match input.advance(1)? {
                Some(enums) => {
                    *engine.durable.enums.write() = enums;
                    Ok(Some(Self::Shells(shell_type::begin_input(
                        Arc::clone(engine.storage.catalog.as_ref().expect("persistent catalog")),
                        engine.durable.roles.read().clone(),
                        limits,
                        engine.durable.shells.read().budget(),
                        control,
                    )?)))
                }
                None => Ok(Some(Self::Enums(input))),
            },
            Self::Shells(mut input) => match input.advance(1)? {
                Some(shells) => {
                    *engine.durable.shells.write() = shells;
                    Ok(None)
                }
                None => Ok(Some(Self::Shells(input))),
            },
        }
    }
}
