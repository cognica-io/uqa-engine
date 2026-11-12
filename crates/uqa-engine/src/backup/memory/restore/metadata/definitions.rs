//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Table input retains owning-crate continuations and publishes only complete metadata units.

use super::{
    invalid, sql_error, BoundTableSecurity, ColumnDef, DroppedAttribute, ForeignKey,
    ImageDecodeWindow, MetadataInput, Read, StatisticsMaintenance, StorageBackendError,
    StorageBackendResult, StorageReadControl, TableCheck, TableDefinition, TableHierarchy,
    TableKeyConstraint, TableMetadata,
};
use uqa_core::image_io::{ImageDecoder, ImageIoError};
use uqa_sql::SQLError;

type Definition<T> = ImageDecoder<TableDefinition<T>, SQLError>;
type NativeMetadata<T> = ImageDecoder<TableMetadata<T>, SQLError>;

pub(super) enum DefinitionInput {
    Security(NativeMetadata<BoundTableSecurity>),
    Columns(Definition<Vec<ColumnDef>>),
    Checks(Definition<Vec<TableCheck>>),
    ForeignKeys(NativeMetadata<Vec<ForeignKey>>),
    Keys(NativeMetadata<Vec<TableKeyConstraint>>),
    Hierarchy(Definition<TableHierarchy>),
    Dropped(NativeMetadata<Vec<DroppedAttribute>>),
    Statistics(ImageDecoder<uqa_planner::ColumnStatistics, SQLError>),
    Maintenance(ImageDecoder<StatisticsMaintenance>),
}

fn maintenance_error(error: ImageIoError) -> StorageBackendError {
    StorageBackendError::backend("memory table maintenance", error)
}

impl DefinitionInput {
    pub(super) fn new(slot: u8, control: &StorageReadControl) -> StorageBackendResult<Self> {
        macro_rules! input {
            ($variant:ident, $ty:ty) => {
                Self::$variant(
                    <$ty>::image_input(control.memory(), control.cancellation())
                        .map_err(sql_error)?,
                )
            };
        }
        Ok(match slot {
            1 => input!(Security, TableMetadata<BoundTableSecurity>),
            2 => input!(Columns, TableDefinition<Vec<ColumnDef>>),
            3 => input!(Checks, TableDefinition<Vec<TableCheck>>),
            4 => input!(ForeignKeys, TableMetadata<Vec<ForeignKey>>),
            5 => input!(Keys, TableMetadata<Vec<TableKeyConstraint>>),
            6 => input!(Hierarchy, TableDefinition<TableHierarchy>),
            7 => input!(Dropped, TableMetadata<Vec<DroppedAttribute>>),
            8 => input!(Statistics, uqa_planner::ColumnStatistics),
            9 => Self::Maintenance(
                StatisticsMaintenance::image_input(control.memory(), control.cancellation())
                    .map_err(maintenance_error)?,
            ),
            _ => return Err(invalid()),
        })
    }

    pub(super) fn advance(
        &mut self,
        state: &mut MetadataInput,
        reader: &mut dyn Read,
        window: ImageDecodeWindow,
    ) -> StorageBackendResult<bool> {
        macro_rules! publish {
            ($input:ident, $field:ident) => {
                let Some(value) = $input.advance(reader, window).map_err(sql_error)? else {
                    return Ok(false);
                };
                state.$field = Some(value);
            };
        }
        match self {
            Self::Security(input) => {
                publish!(input, security);
            }
            Self::Columns(input) => {
                publish!(input, columns);
            }
            Self::Checks(input) => {
                publish!(input, checks);
            }
            Self::ForeignKeys(input) => {
                publish!(input, foreign_keys);
            }
            Self::Keys(input) => {
                publish!(input, keys);
            }
            Self::Hierarchy(input) => {
                publish!(input, hierarchy);
            }
            Self::Dropped(input) => {
                publish!(input, dropped);
            }
            Self::Statistics(input) => {
                let Some(value) = input.advance(reader, window).map_err(sql_error)? else {
                    return Ok(false);
                };
                state.statistics = Some(value);
            }
            Self::Maintenance(input) => {
                let Some(value) = input.advance(reader, window).map_err(maintenance_error)? else {
                    return Ok(false);
                };
                state.maintenance = Some(value);
            }
        }
        Ok(true)
    }
}
