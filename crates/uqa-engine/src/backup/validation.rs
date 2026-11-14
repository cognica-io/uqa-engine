//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One immutable catalog view connects native image validation to every Engine destination.

use crate::Engine;
use uqa_storage::{backup::BackupResult, read_control::StorageReadControl, StorageBackendError};

pub(super) fn validate(engine: &Engine, control: &StorageReadControl) -> BackupResult<()> {
    let production = uqa_core::memory::ProductionControl::new(
        control.memory(),
        control.cancellation(),
        control.cancellation(),
    );
    let catalog = engine
        .restored_catalog_read_view_with_control(&production)
        .map_err(sql_error)?;
    uqa_execution::catalog::image_validation::validate_namespaces(&catalog, &production)
        .map_err(sql_error)?;
    uqa_execution::catalog::image_validation::validate_name_uniqueness(
        catalog.snapshot(),
        control.memory(),
        control.cancellation(),
    )
    .map_err(sql_error)?;
    uqa_execution::catalog::image_validation::validate_authority(
        catalog.snapshot(),
        control.memory(),
        control.cancellation(),
    )
    .map_err(sql_error)?;
    uqa_execution::catalog::image_validation::validate_types(
        catalog.snapshot(),
        control.memory(),
        control.cancellation(),
    )
    .map_err(sql_error)?;
    uqa_execution::catalog::image_validation::validate_plan_relations(&catalog, &production)
        .map_err(sql_error)?;
    uqa_execution::catalog::image_validation::validate_hierarchies(
        catalog.snapshot(),
        control.memory(),
        control.cancellation(),
    )
    .map_err(sql_error)?;
    uqa_execution::catalog::image_validation::validate_sequences(
        catalog.snapshot(),
        control.memory(),
        control.cancellation(),
    )
    .map_err(sql_error)?;
    uqa_execution::catalog::image_validation::validate_indexes(
        catalog.snapshot(),
        control.memory(),
        control.cancellation(),
    )
    .map_err(sql_error)?;
    uqa_execution::catalog::image_validation::validate_constraints(
        catalog.snapshot(),
        control.memory(),
        control.cancellation(),
    )
    .map_err(sql_error)?;
    uqa_execution::catalog::image_validation::validate_routines(
        catalog.snapshot(),
        control.memory(),
        control.cancellation(),
    )
    .map_err(sql_error)?;
    uqa_execution::catalog::image_validation::validate_sequence_functions(&catalog, &production)
        .map_err(sql_error)?;
    control.check()?;
    Ok(())
}

fn sql_error(error: uqa_sql::SQLError) -> StorageBackendError {
    StorageBackendError::backend("backup catalog validation", error)
}
