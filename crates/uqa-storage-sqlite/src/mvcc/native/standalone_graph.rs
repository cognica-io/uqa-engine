//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical standalone graph conversion and native record format ownership.

mod migration;
pub(super) mod schema;

pub(super) use migration::{import_sources, install_legacy_guards, validate_legacy_guards};

use super::{invalid, physical, NativeRecordFamily as Family};
use crate::mvcc::PhysicalResult;
use rusqlite::{types::ValueRef, Connection};
use uqa_storage::{mvcc::VersionResult, read_control::StorageReadControl};

pub(super) fn validate_values(family: Family, values: &[ValueRef<'_>]) -> VersionResult<()> {
    let scope = values[0]
        .as_str()
        .map_err(|_| invalid("standalone graph scope is not text"))?;
    if !scope
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err(invalid("standalone graph scope is not canonical"));
    }
    if family == Family::StandaloneGraphScopes && values[1] != ValueRef::Null {
        let suffix = values[1]
            .as_str()
            .map_err(|_| invalid("standalone graph legacy suffix is not text"))?;
        let matched = if scope.is_empty() {
            suffix.is_empty()
        } else {
            suffix
                .strip_prefix('_')
                .is_some_and(|name| name.eq_ignore_ascii_case(scope))
        };
        if !matched {
            return Err(invalid(
                "standalone graph legacy suffix disagrees with its scope",
            ));
        }
    }
    Ok(())
}

pub(super) fn validate_owner(
    connection: &Connection,
    values: &[ValueRef<'_>],
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    let key = super::encode_row(&values[..1], control)?;
    if physical::get(
        connection,
        Family::StandaloneGraphScopes.layout(),
        &key,
        control,
    )?
    .is_none()
    {
        return Err(invalid("standalone graph record has no namespace").into());
    }
    Ok(())
}

pub(super) fn validate_target(
    connection: &Connection,
    values: &[ValueRef<'_>],
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    // Only baseline conversion may introduce a legacy source binding and its physical views.
    if values[1] != ValueRef::Null {
        validate_owner(connection, values, control)?;
    }
    Ok(())
}

pub(super) fn create(connection: &Connection) -> PhysicalResult<()> {
    for (_, sql) in schema::TABLES {
        connection.execute_batch(sql)?;
    }
    Ok(())
}

/// Namespace rows and their reserved legacy names publish under the same physical commit; failed history publication rolls both back.
pub(super) fn publish_scope(
    connection: &Connection,
    values: &[ValueRef<'_>],
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    let family = Family::StandaloneGraphScopes;
    let key = super::encode_row(&values[..1], control)?;
    let existing = physical::get(connection, family.layout(), &key, control)?.is_some();
    physical::upsert(connection, family.layout(), values, control)?;
    if !existing {
        migration::install_scope_guards(connection, values, control)?;
    }
    Ok(())
}
