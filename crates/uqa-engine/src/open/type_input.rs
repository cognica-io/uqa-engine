//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The private session retains complete type and comment generations in ordinary restoration order.

use super::Engine;
use std::sync::Arc;
use uqa_execution::catalog::{comments, composite_type, domain, enum_type, range_type, shell_type};
use uqa_storage::{
    catalog::input::CatalogInputLimits, read_control::StorageReadControl, StorageBackendResult,
};

pub(super) enum PersistentTypeInput {
    Enums(enum_type::EnumCatalogInput),
    Shells(shell_type::ShellCatalogInput),
    Ranges(range_type::RangeCatalogInput),
    Comments(comments::CommentCatalogInput),
    Composites(composite_type::CompositeCatalogInput),
    Domains(domain::DomainCatalogInput),
}

pub(super) enum TypeProgress {
    Pending(PersistentTypeInput),
    Complete(domain::DomainRestoreState),
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
    ) -> StorageBackendResult<TypeProgress> {
        match self {
            Self::Enums(mut input) => match input.advance(1)? {
                Some(enums) => {
                    *engine.durable.enums.write() = enums;
                    Ok(TypeProgress::Pending(Self::Shells(
                        shell_type::begin_input(
                            Arc::clone(
                                engine.storage.catalog.as_ref().expect("persistent catalog"),
                            ),
                            engine.durable.roles.read().clone(),
                            limits,
                            engine.durable.shells.read().budget(),
                            control,
                        )?,
                    )))
                }
                None => Ok(TypeProgress::Pending(Self::Enums(input))),
            },
            Self::Shells(mut input) => match input.advance(1)? {
                Some(shells) => {
                    *engine.durable.shells.write() = shells;
                    Ok(TypeProgress::Pending(Self::ranges(
                        engine, limits, control,
                    )?))
                }
                None => Ok(TypeProgress::Pending(Self::Shells(input))),
            },
            Self::Ranges(mut input) => match input.advance(1)? {
                Some(ranges) => {
                    *engine.durable.ranges.write() = ranges;
                    Ok(TypeProgress::Pending(Self::comments(
                        engine, limits, control,
                    )?))
                }
                None => Ok(TypeProgress::Pending(Self::Ranges(input))),
            },
            Self::Comments(mut input) => match input.advance(1)? {
                Some(comments) => {
                    *engine.durable.comments.write() = comments;
                    Ok(TypeProgress::Pending(Self::composites(
                        engine, limits, control,
                    )?))
                }
                None => Ok(TypeProgress::Pending(Self::Comments(input))),
            },
            Self::Composites(mut input) => match input.advance(1)? {
                Some(composites) => {
                    *engine.durable.composites.write() = composites;
                    Ok(TypeProgress::Pending(Self::domains(
                        engine, limits, control,
                    )?))
                }
                None => Ok(TypeProgress::Pending(Self::Composites(input))),
            },
            Self::Domains(mut input) => match input.advance(1)? {
                Some(domains) => {
                    *engine.durable.domains.write() = domains.registry;
                    Ok(TypeProgress::Complete(domains.state))
                }
                None => Ok(TypeProgress::Pending(Self::Domains(input))),
            },
        }
    }

    fn ranges(
        engine: &Engine,
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        Ok(Self::Ranges(range_type::begin_input(
            Arc::clone(engine.storage.catalog.as_ref().expect("persistent catalog")),
            engine.durable.roles.read().clone(),
            limits,
            engine.durable.ranges.read().budget(),
            control,
        )?))
    }

    fn domains(
        engine: &Engine,
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        Ok(Self::Domains(domain::DomainCatalogInput::new(
            Arc::clone(engine.storage.catalog.as_ref().expect("persistent catalog")),
            engine.durable.roles.read().clone(),
            limits,
            engine.durable.domains.read().budget(),
            control,
        )?))
    }
    fn comments(
        engine: &Engine,
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        Ok(Self::Comments(comments::CommentCatalogInput::new(
            Arc::clone(engine.storage.catalog.as_ref().expect("persistent catalog")),
            limits,
            engine.durable.comments.read().budget(),
            control,
        )?))
    }
    fn composites(
        engine: &Engine,
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        Ok(Self::Composites(composite_type::begin_input(
            Arc::clone(engine.storage.catalog.as_ref().expect("persistent catalog")),
            engine.durable.roles.read().clone(),
            limits,
            engine.durable.composites.read().budget(),
            control,
        )?))
    }
}
