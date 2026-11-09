//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered native catalogs register their owning immutable generations in the fresh Engine.

use crate::state::{CatalogCell, CatalogGeneration, DurableCatalogState};
use std::io::Read;
use uqa_core::memory::MemoryBudget;
use uqa_execution::catalog::sequence::registry::{SequenceCatalog, SequenceCatalogInput};
use uqa_sql::catalog;
use uqa_storage::{
    memory_capture::MemoryCaptureRegistry, read_control::StorageReadControl, StorageBackendError,
    StorageBackendResult,
};

#[cfg(test)]
mod tests;

pub(super) struct CatalogInput {
    next: u16,
    sequence: Option<SequenceCatalogInput>,
}

pub(super) struct Destination<'a> {
    pub registry: &'a MemoryCaptureRegistry,
    pub mutations: &'a MemoryBudget,
    pub control: &'a StorageReadControl,
}

impl CatalogInput {
    pub(super) fn new() -> Self {
        Self {
            next: 1,
            sequence: None,
        }
    }

    pub(super) fn read(
        mut self,
        state: &mut DurableCatalogState,
        slot: u16,
        reader: &mut dyn Read,
        destination: &Destination<'_>,
    ) -> StorageBackendResult<Self> {
        let Destination {
            registry,
            mutations,
            control,
        } = *destination;
        control.check()?;
        if slot != self.next {
            return Err(invalid());
        }
        macro_rules! resource {
            ($field:ident, $kind:ty, $capture:expr) => {{
                let value = <$kind>::read_image_with_mutation_budget(
                    reader,
                    control.memory(),
                    mutations,
                    control.cancellation(),
                )
                .map_err(super::metadata::sql_error)?;
                state.$field = CatalogCell::capture_resource_with_control(
                    CatalogGeneration::with_budget(value, control.memory())?,
                    registry,
                    $capture,
                    control,
                )?;
            }};
        }
        match slot {
            1..=18 | 23 => read_definition(state, slot, reader, destination)?,
            19 => {
                self.sequence = Some(
                    SequenceCatalogInput::read_metadata(
                        reader,
                        control.memory(),
                        control.cancellation(),
                    )
                    .map_err(super::metadata::sql_error)?,
                );
            }
            20 => {
                let value = self
                    .sequence
                    .take()
                    .ok_or_else(invalid)?
                    .read_authority(reader, mutations)
                    .map_err(super::metadata::sql_error)?;
                state.sequences = CatalogCell::capture_source_with_control(
                    CatalogGeneration::with_budget(value, control.memory())?,
                    registry,
                    SequenceCatalog::capture,
                    control,
                )?;
            }
            21 => {
                let value = uqa_execution::catalog::foreign::ForeignCatalog::read_image_with_mutation_budget(reader, control, mutations).map_err(super::metadata::sql_error)?;
                state.foreign_catalog = CatalogCell::capture_source_with_control(
                    CatalogGeneration::with_budget(value, control.memory())?,
                    registry,
                    uqa_execution::catalog::foreign::ForeignCatalog::capture,
                    control,
                )?;
            }
            22 => resource!(
                catalog_indexes,
                uqa_execution::catalog::index::registry::CatalogIndexes,
                uqa_execution::catalog::index::registry::CatalogIndexes::capture
            ),
            24 => {
                let value = uqa_storage::catalog::scoring_params::ScoringParameters::read_image(
                    reader, control,
                )?
                .into_mutation_budget(mutations);
                state.scoring_params = CatalogCell::capture_resource_with_control(
                    CatalogGeneration::with_budget(value, control.memory())?,
                    registry,
                    uqa_storage::catalog::scoring_params::ScoringParameters::capture,
                    control,
                )?;
            }
            _ => return Err(invalid()),
        }
        control.check()?;
        self.next += 1;
        Ok(self)
    }

    pub(super) fn finish(self) -> StorageBackendResult<()> {
        if self.next != uqa_storage::backup::MEMORY_IMAGE_CATALOG_COUNT + 1
            || self.sequence.is_some()
        {
            return Err(invalid());
        }
        Ok(())
    }
}

fn read_definition(
    state: &mut DurableCatalogState,
    slot: u16,
    reader: &mut dyn Read,
    destination: &Destination<'_>,
) -> StorageBackendResult<()> {
    let Destination {
        registry,
        mutations,
        control,
    } = *destination;
    macro_rules! definition {
        ($field:ident, $kind:ty) => {{
            let value = <$kind>::read_image_with_mutation_budget(
                reader,
                control.memory(),
                mutations,
                control.cancellation(),
            )
            .map_err(super::metadata::sql_error)?;
            state.$field = CatalogCell::capture_source_with_control(
                CatalogGeneration::with_budget(value, control.memory())?,
                registry,
                <$kind>::capture,
                control,
            )?;
        }};
    }
    match slot {
        1 => definition!(
            domains,
            catalog::type_definitions::TypeDefinitions<catalog::domain::StoredDomain>
        ),
        2 => definition!(enums, uqa_execution::catalog::enum_type::EnumRegistry),
        3 => definition!(
            composites,
            uqa_execution::catalog::composite_type::CompositeRegistry
        ),
        4 => definition!(ranges, uqa_execution::catalog::range_type::RangeRegistry),
        5 => definition!(shells, uqa_execution::catalog::shell_type::ShellRegistry),
        6 => definition!(schemas, catalog::security::schema_registry::SchemaRegistry),
        7 => definition!(views, catalog::stored_view::ViewDefinitions),
        8 => definition!(
            sql_user_functions,
            uqa_sql::routines::registry::RoutineRegistry
        ),
        9 => definition!(roles, catalog::roles::RoleDefinitions),
        10 => definition!(role_memberships, catalog::roles::RoleMemberships),
        11 => definition!(
            database_security,
            catalog::security::database::DatabaseAuthority
        ),
        12 => definition!(
            builtin_routine_security,
            catalog::security::builtin_routines::BuiltinRoutineSecurities
        ),
        13 => definition!(
            system_relation_security,
            catalog::security::system_relations::SystemRelationSecurities
        ),
        14 => definition!(foreign_wrappers, catalog::foreign_wrapper::ForeignWrappers),
        15 => definition!(foreign_servers, catalog::foreign_server::ForeignServers),
        16 => definition!(triggers, catalog::events::TriggerCatalog),
        17 => definition!(rules, catalog::events::RuleCatalog),
        18 => definition!(graph_catalog_oids, catalog::graph_oids::GraphOidRegistry),
        23 => definition!(comments, uqa_execution::catalog::comments::CommentRegistry),
        _ => return Err(invalid()),
    }
    Ok(())
}

fn invalid() -> StorageBackendError {
    StorageBackendError::Other("incomplete or unordered native catalog images".into())
}
