//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog slots select the original owner; encoding remains in SQL, Execution or Storage.

use super::readers::{captured, sql_error, ReadFn};
use crate::state::SnapshotCut;
use uqa_storage::{image_io::ImageReader, read_control::StorageReadControl, StorageBackendResult};

pub(super) const CATALOG_COUNT: u16 = 24;

pub(super) fn image(
    source: &SnapshotCut,
    slot: u16,
    control: &StorageReadControl,
) -> StorageBackendResult<Option<ImageReader<'static>>> {
    macro_rules! sql_catalog {
        ($key:expr) => {
            captured($key, &source.cut, control, |value, control| {
                let mut reader = value
                    .image_reader(control.cancellation())
                    .map_err(sql_error)?;
                ImageReader::new(
                    ReadFn(move |bytes: &mut [u8]| reader.read(bytes).map_err(sql_error)),
                    control,
                )
            })?
        };
    }
    macro_rules! sql_definition {
        ($key:expr) => {
            captured($key, &source.cut, control, |value, control| {
                let mut reader = value
                    .image_reader(control.memory(), control.cancellation())
                    .map_err(sql_error)?;
                ImageReader::new(
                    ReadFn(move |bytes: &mut [u8]| reader.read(bytes).map_err(sql_error)),
                    control,
                )
            })?
        };
    }
    macro_rules! native {
        ($key:expr) => {
            captured($key, &source.cut, control, |value, control| {
                value.image_reader(control)
            })?
        };
    }
    source.cut.check()?;
    let reader = match slot {
        1 => sql_definition!(&source.catalogs.domains),
        2 => sql_catalog!(&source.catalogs.enums),
        3 => sql_catalog!(&source.catalogs.composites),
        4 => sql_catalog!(&source.catalogs.ranges),
        5 => sql_catalog!(&source.catalogs.shells),
        6 => sql_catalog!(&source.catalogs.schemas),
        7 => sql_definition!(&source.catalogs.views),
        8 => sql_definition!(&source.catalogs.sql_user_functions),
        9 => sql_catalog!(&source.catalogs.roles),
        10 => sql_catalog!(&source.catalogs.role_memberships),
        11 => sql_catalog!(&source.catalogs.database_security),
        12 => sql_catalog!(&source.catalogs.builtin_routine_security),
        13 => sql_catalog!(&source.catalogs.system_relation_security),
        14 => sql_catalog!(&source.catalogs.foreign_wrappers),
        15 => sql_catalog!(&source.catalogs.foreign_servers),
        16 => sql_definition!(&source.catalogs.triggers),
        17 => sql_definition!(&source.catalogs.rules),
        18 => sql_catalog!(&source.catalogs.graph_catalog_oids),
        19 => sql_catalog!(&source.sequences),
        20 => captured(&source.sequences, &source.cut, control, |value, control| {
            let mut reader = value
                .authority_image_reader(control.cancellation())
                .map_err(sql_error)?;
            ImageReader::new(
                ReadFn(move |bytes: &mut [u8]| reader.read(bytes).map_err(sql_error)),
                control,
            )
        })?,
        21 => native!(&source.foreign),
        22 => captured(&source.metadata, &source.cut, control, |value, control| {
            value.indexes.image_reader(control)
        })?,
        23 => native!(&source.comments),
        24 => native!(&source.resources.scoring_params),
        _ => return Ok(None),
    };
    source.cut.check()?;
    Ok(Some(reader))
}
