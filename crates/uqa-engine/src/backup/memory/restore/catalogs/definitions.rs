//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained owning-crate decoders publish only complete native catalog generations.

use super::{
    invalid, CatalogCell, CatalogGeneration, Destination, DurableCatalogState, Read,
    StorageBackendResult,
};
use uqa_core::image_io::ImageDecoder;
use uqa_execution::catalog::{
    comments::CommentRegistry, composite_type::CompositeRegistry, domain::DomainRegistry,
    enum_type::EnumRegistry, foreign::ForeignCatalog, index::registry::CatalogIndexes,
    range_type::RangeRegistry, shell_type::ShellRegistry,
};
use uqa_sql::catalog::{
    events::{RuleCatalog, TriggerCatalog},
    foreign_server::ForeignServers,
    foreign_wrapper::ForeignWrappers,
    graph_oids::GraphOidRegistry,
    image::CatalogImageInput,
    roles::{RoleDefinitions, RoleMemberships},
    security::{
        builtin_routines::BuiltinRoutineSecurities, database::DatabaseAuthority,
        schema_registry::SchemaRegistry, system_relations::SystemRelationSecurities,
    },
    stored_view::ViewDefinitions,
};
use uqa_sql::{routines::registry::RoutineRegistry, SQLError};
use uqa_storage::{catalog::scoring_params::ScoringParameters, StorageBackendError};

pub(super) enum DefinitionInput {
    Domains(CatalogImageInput<DomainRegistry>),
    Views(CatalogImageInput<ViewDefinitions>),
    Routines(CatalogImageInput<RoutineRegistry>),
    Enums(CatalogImageInput<EnumRegistry>),
    Composites(CatalogImageInput<CompositeRegistry>),
    Ranges(CatalogImageInput<RangeRegistry>),
    Shells(CatalogImageInput<ShellRegistry>),
    Schemas(CatalogImageInput<SchemaRegistry>),
    Roles(CatalogImageInput<RoleDefinitions>),
    Memberships(CatalogImageInput<RoleMemberships>),
    Database(CatalogImageInput<DatabaseAuthority>),
    BuiltinRoutines(CatalogImageInput<BuiltinRoutineSecurities>),
    SystemRelations(CatalogImageInput<SystemRelationSecurities>),
    ForeignWrappers(CatalogImageInput<ForeignWrappers>),
    ForeignServers(CatalogImageInput<ForeignServers>),
    GraphOids(CatalogImageInput<GraphOidRegistry>),
    Indexes(ImageDecoder<CatalogIndexes, SQLError>),
    Comments(ImageDecoder<CommentRegistry, SQLError>),
    Scoring(ImageDecoder<ScoringParameters, StorageBackendError>),
    Triggers(ImageDecoder<TriggerCatalog, SQLError>),
    Rules(ImageDecoder<RuleCatalog, SQLError>),
    ForeignTables(ImageDecoder<ForeignCatalog, SQLError>),
}

impl DefinitionInput {
    pub(super) fn new(slot: u16, destination: &Destination<'_>) -> StorageBackendResult<Self> {
        let Destination {
            mutations, control, ..
        } = *destination;
        macro_rules! input {
            ($variant:ident, $ty:ty) => {
                Self::$variant(
                    <$ty>::image_input(control.memory(), mutations, control.cancellation())
                        .map_err(super::super::metadata::sql_error)?,
                )
            };
        }
        Ok(match slot {
            1 => input!(Domains, DomainRegistry),
            2 => input!(Enums, EnumRegistry),
            3 => input!(Composites, CompositeRegistry),
            4 => input!(Ranges, RangeRegistry),
            5 => input!(Shells, ShellRegistry),
            6 => input!(Schemas, SchemaRegistry),
            7 => input!(Views, ViewDefinitions),
            8 => input!(Routines, RoutineRegistry),
            9 => input!(Roles, RoleDefinitions),
            10 => input!(Memberships, RoleMemberships),
            11 => input!(Database, DatabaseAuthority),
            12 => input!(BuiltinRoutines, BuiltinRoutineSecurities),
            13 => input!(SystemRelations, SystemRelationSecurities),
            14 => input!(ForeignWrappers, ForeignWrappers),
            15 => input!(ForeignServers, ForeignServers),
            16 => input!(Triggers, TriggerCatalog),
            17 => input!(Rules, RuleCatalog),
            18 => input!(GraphOids, GraphOidRegistry),
            21 => input!(ForeignTables, ForeignCatalog),
            22 => input!(Indexes, CatalogIndexes),
            23 => input!(Comments, CommentRegistry),
            24 => Self::Scoring(ScoringParameters::image_input(control, mutations)?),
            _ => return Err(invalid()),
        })
    }

    pub(super) fn advance(
        &mut self,
        state: &mut DurableCatalogState,
        reader: &mut dyn Read,
        destination: &Destination<'_>,
    ) -> StorageBackendResult<bool> {
        let registry = destination.registry;
        let control = destination.control;
        macro_rules! publish {
            ($input:ident, $field:ident, $ty:ty) => {{
                let Some(value) = $input
                    .advance_with_window(reader, 1, destination.window)
                    .map_err(super::super::metadata::sql_error)?
                else {
                    return Ok(false);
                };
                state.$field = CatalogCell::capture_source_with_control(
                    CatalogGeneration::with_budget(value, control.memory())?,
                    registry,
                    <$ty>::capture,
                    Some(control),
                )?;
            }};
        }
        macro_rules! native {
            ($decoded:expr, $field:ident, $ty:ty, $capture:ident) => {{
                let Some(value) = $decoded? else {
                    return Ok(false);
                };
                state.$field = CatalogCell::$capture(
                    CatalogGeneration::with_budget(value, control.memory())?,
                    registry,
                    <$ty>::capture,
                    Some(control),
                )?;
            }};
        }
        match self {
            Self::Domains(input) => publish!(input, domains, DomainRegistry),
            Self::Enums(input) => publish!(input, enums, EnumRegistry),
            Self::Composites(input) => publish!(input, composites, CompositeRegistry),
            Self::Ranges(input) => publish!(input, ranges, RangeRegistry),
            Self::Shells(input) => publish!(input, shells, ShellRegistry),
            Self::Schemas(input) => publish!(input, schemas, SchemaRegistry),
            Self::Views(input) => publish!(input, views, ViewDefinitions),
            Self::Routines(input) => publish!(input, sql_user_functions, RoutineRegistry),
            Self::Roles(input) => publish!(input, roles, RoleDefinitions),
            Self::Memberships(input) => publish!(input, role_memberships, RoleMemberships),
            Self::Database(input) => publish!(input, database_security, DatabaseAuthority),
            Self::BuiltinRoutines(input) => {
                publish!(input, builtin_routine_security, BuiltinRoutineSecurities);
            }
            Self::SystemRelations(input) => {
                publish!(input, system_relation_security, SystemRelationSecurities);
            }
            Self::ForeignWrappers(input) => publish!(input, foreign_wrappers, ForeignWrappers),
            Self::ForeignServers(input) => publish!(input, foreign_servers, ForeignServers),
            Self::GraphOids(input) => publish!(input, graph_catalog_oids, GraphOidRegistry),
            Self::Indexes(input) => native!(
                input
                    .advance(reader, destination.window)
                    .map_err(super::super::metadata::sql_error),
                catalog_indexes,
                CatalogIndexes,
                capture_resource_with_control
            ),
            Self::Triggers(input) => native!(
                input
                    .advance(reader, destination.window)
                    .map_err(super::super::metadata::sql_error),
                triggers,
                TriggerCatalog,
                capture_source_with_control
            ),
            Self::ForeignTables(input) => native!(
                input
                    .advance(reader, destination.window)
                    .map_err(super::super::metadata::sql_error),
                foreign_catalog,
                ForeignCatalog,
                capture_source_with_control
            ),
            Self::Rules(input) => native!(
                input
                    .advance(reader, destination.window)
                    .map_err(super::super::metadata::sql_error),
                rules,
                RuleCatalog,
                capture_source_with_control
            ),
            Self::Comments(input) => native!(
                input
                    .advance(reader, destination.window)
                    .map_err(super::super::metadata::sql_error),
                comments,
                CommentRegistry,
                capture_source_with_control
            ),
            Self::Scoring(input) => native!(
                input.advance(reader, destination.window),
                scoring_params,
                ScoringParameters,
                capture_resource_with_control
            ),
        }
        Ok(true)
    }
}
