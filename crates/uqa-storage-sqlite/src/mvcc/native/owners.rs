//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable name-to-generation bindings. Exact standalone API names remain distinct from canonical SQL relation names.

use rusqlite::{
    params,
    types::{ToSqlOutput, ValueRef},
    Connection, OptionalExtension,
};
use uqa_core::memory::{BudgetedVec, MemoryError};
use uqa_storage::mvcc::{DatabaseId, VersionError};
use uqa_storage::read_control::StorageReadControl;

use super::{
    invalid, physical, NativeRecordFamily as Family, NativeRecordIdentity, NativeRecordOwner,
};
use crate::mvcc::PhysicalResult;

fn nonzero(value: ValueRef<'_>) -> PhysicalResult<[u8; 16]> {
    let ValueRef::Blob(bytes) = value else {
        return Err(invalid("native owner must be a BLOB").into());
    };
    let bytes: [u8; 16] = bytes
        .try_into()
        .map_err(|_| invalid("invalid native owner length"))?;
    if bytes == [0; 16] {
        return Err(invalid("native owner cannot be zero").into());
    }
    Ok(bytes)
}

pub(super) fn allocate(value: ValueRef<'_>) -> PhysicalResult<[u8; 16]> {
    if let ValueRef::Blob(bytes) = value {
        if !bytes.is_empty() && bytes != [0; 16] {
            return nonzero(value);
        }
    } else {
        return nonzero(value);
    }
    let mut bytes = [0; 16];
    while bytes == [0; 16] {
        getrandom::fill(&mut bytes).map_err(|error| {
            VersionError::Storage(
                crate::SQLiteError::Io(std::io::Error::other(error.to_string())).into(),
            )
        })?;
    }
    Ok(bytes)
}

fn definition(family: Family, values: &[ValueRef<'_>]) -> PhysicalResult<NativeRecordOwner> {
    let layout = family.layout();
    let column = |name| {
        layout
            .columns
            .iter()
            .position(|column| *column == name)
            .expect("definition layout column")
    };
    Ok(NativeRecordOwner::Object {
        identity: nonzero(values[column("object_id")])?,
        generation: nonzero(
            values[column(if family == Family::Tables {
                "storage_generation"
            } else {
                "definition_generation"
            })],
        )?,
    })
}

pub(super) fn canonical_name(
    values: &[ValueRef<'_>],
    control: &StorageReadControl,
) -> PhysicalResult<BudgetedVec<u8>> {
    let schema = values[0]
        .as_str()
        .map_err(|_| invalid("native definition schema must be text"))?;
    let relation = values[1]
        .as_str()
        .map_err(|_| invalid("native definition name must be text"))?;
    let reserve = schema
        .len()
        .checked_add(relation.len())
        .and_then(|size| size.checked_mul(5))
        .and_then(|size| size.checked_add(16))
        .ok_or(VersionError::from(MemoryError::SizeOverflow))?;
    let _render = control
        .memory()
        .reserve(reserve)
        .map_err(VersionError::from)?;
    let rendered = uqa_storage::RelationIdentity::new(schema, relation).qualified_name();
    let mut name = BudgetedVec::new(control.memory());
    name.extend_from_slice(rendered.as_bytes())
        .map_err(VersionError::from)?;
    Ok(name)
}

pub(super) fn lookup(
    connection: &Connection,
    name: ValueRef<'_>,
    control: &StorageReadControl,
) -> PhysicalResult<Option<NativeRecordOwner>> {
    let _bindings = physical::reserve_values(&[name], control)?;
    let mut statement = connection
        .prepare("SELECT object_id, generation FROM _uqa_mvcc_native_owners WHERE name = ?1")?;
    let mut rows = statement.query(params![ToSqlOutput::Borrowed(name)])?;
    rows.next()?
        .map(|row| {
            Ok(NativeRecordOwner::Object {
                identity: nonzero(row.get_ref(0)?)?,
                generation: nonzero(row.get_ref(1)?)?,
            })
        })
        .transpose()
}

pub(super) fn for_row(
    connection: &Connection,
    database: DatabaseId,
    family: Family,
    values: &[ValueRef<'_>],
    control: &StorageReadControl,
) -> PhysicalResult<NativeRecordOwner> {
    if !family.layout().object_owned {
        return Ok(NativeRecordOwner::Database(database));
    }
    if matches!(family, Family::Tables | Family::Sequences) {
        return definition(family, values);
    }
    let column = family
        .layout()
        .columns
        .iter()
        .position(|column| *column == "table_name")
        .expect("table-owned layout");
    lookup(connection, values[column], control)?
        .ok_or_else(|| invalid("native row has no table owner").into())
}

pub(super) fn validate(
    connection: &Connection,
    database: DatabaseId,
    identity: NativeRecordIdentity,
    values: &[ValueRef<'_>],
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    let family = identity.family();
    if family == Family::GraphLookups {
        super::graph_lookup::validate_row(connection, values, control)?;
    }
    if for_row(connection, database, family, values, control)? != identity.owner() {
        return Err(invalid("native row targets a different active owner generation").into());
    }
    if family == Family::Tables {
        let name = canonical_name(values, control)?;
        if lookup(connection, ValueRef::Text(&name), control)? != Some(identity.owner()) {
            return Err(invalid("native table definition disagrees with its owner binding").into());
        }
        let _bindings = crate::read_control::reserve_bindings(control, &[&name])?;
        if !connection.query_row(
            "SELECT catalog_owned = 1 FROM _uqa_mvcc_native_owners WHERE name = ?1",
            params![std::str::from_utf8(&name).expect("canonical UTF-8 name")],
            |row| row.get::<_, bool>(0),
        )? {
            return Err(invalid("native table definition requires a catalog-owned binding").into());
        }
    }
    if family == Family::TableOwners {
        let owner = NativeRecordOwner::Object {
            identity: nonzero(values[1])?,
            generation: nonzero(values[2])?,
        };
        if !matches!(values[3], ValueRef::Integer(0 | 1)) {
            return Err(invalid("invalid native table owner kind").into());
        }
        if values[3] == ValueRef::Integer(1) {
            let name = values[0]
                .as_str()
                .map_err(|_| invalid("native owner name is not text"))?;
            let _names = control
                .memory()
                .reserve(
                    name.len()
                        .checked_mul(4)
                        .ok_or(VersionError::from(MemoryError::SizeOverflow))?,
                )
                .map_err(VersionError::from)?;
            let relation = uqa_storage::RelationIdentity::from_legacy_name(name)
                .map_err(|_| invalid("invalid native catalog owner name"))?;
            if relation.qualified_name() != name {
                return Err(invalid("catalog owner name is not canonical").into());
            }
            let key = super::encode_row(
                &[
                    ValueRef::Text(relation.schema.as_bytes()),
                    ValueRef::Text(relation.name.as_bytes()),
                ],
                control,
            )?;
            let row = physical::get(connection, Family::Tables.layout(), &key, control)?
                .ok_or_else(|| invalid("catalog owner lacks its table definition"))?;
            let columns = super::decode_row(&row, Family::Tables.layout().columns.len(), control)?;
            if definition(Family::Tables, &columns)? != owner {
                return Err(invalid("catalog owner disagrees with table definition").into());
            }
        }
    }
    Ok(())
}

pub(super) fn seed(connection: &Connection, control: &StorageReadControl) -> PhysicalResult<()> {
    for family in [Family::Tables, Family::Sequences] {
        let layout = family.layout();
        physical::visit(connection, layout, control, |values| {
            let id_slot = layout
                .columns
                .iter()
                .position(|column| *column == "object_id")
                .expect("object id");
            let generation_slot = layout
                .columns
                .iter()
                .position(|column| {
                    *column
                        == if family == Family::Tables {
                            "storage_generation"
                        } else {
                            "definition_generation"
                        }
                })
                .expect("generation");
            let id = allocate(values[id_slot])?;
            let generation = allocate(values[generation_slot])?;
            if values[id_slot] != ValueRef::Blob(&id)
                || values[generation_slot] != ValueRef::Blob(&generation)
            {
                let mut updated = BudgetedVec::new(control.memory());
                updated
                    .extend_from_slice(values)
                    .map_err(VersionError::from)?;
                updated[id_slot] = ValueRef::Blob(&id);
                updated[generation_slot] = ValueRef::Blob(&generation);
                physical::upsert(connection, layout, &updated, control)?;
            }
            if family == Family::Tables {
                let name = canonical_name(values, control)?;
                physical::upsert(
                    connection,
                    Family::TableOwners.layout(),
                    &[
                        ValueRef::Text(&name),
                        ValueRef::Blob(&id),
                        ValueRef::Blob(&generation),
                        ValueRef::Integer(1),
                    ],
                    control,
                )?;
            }
            Ok(())
        })?;
    }
    for family in Family::all() {
        let layout = family.layout();
        let Some(column) = layout
            .columns
            .iter()
            .position(|column| *column == "table_name")
        else {
            continue;
        };
        physical::visit(connection, layout, control, |values| {
            if lookup(connection, values[column], control)?.is_none() {
                let id = allocate(ValueRef::Blob(&[]))?;
                let generation = allocate(ValueRef::Blob(&[]))?;
                physical::upsert(
                    connection,
                    Family::TableOwners.layout(),
                    &[
                        values[column],
                        ValueRef::Blob(&id),
                        ValueRef::Blob(&generation),
                        ValueRef::Integer(0),
                    ],
                    control,
                )?;
            }
            Ok(())
        })?;
    }
    Ok(())
}

/// A retired generation must include every original physical row in its evaluated batch. Otherwise rows could survive under a new owner while retaining the old MVCC identity.
pub(super) fn validate_retirement(
    connection: &Connection,
    name: ValueRef<'_>,
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    for family in Family::all() {
        let layout = family.layout();
        if !layout.columns.contains(&"table_name") {
            continue;
        }
        control.cancellation().check().map_err(VersionError::from)?;
        let arguments = layout
            .primary_key
            .iter()
            .map(|&slot| format!("source.\"{}\"", layout.columns[slot]))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!("SELECT EXISTS(SELECT 1 FROM {} AS source WHERE source.table_name = ?1 AND NOT EXISTS(SELECT 1 FROM _uqa_mvcc_native_expected AS expected WHERE expected.family = {} AND expected.physical_key = __uqa_mvcc_native_key({}, {arguments}) AND expected.old_key IS NOT NULL))", layout.table, family.id(), family.id());
        let _bindings = physical::reserve_values(&[name], control)?;
        if connection.query_row(&sql, params![ToSqlOutput::Borrowed(name)], |row| {
            row.get::<_, bool>(0)
        })? {
            return Err(
                invalid("native owner change omits live rows from its old generation").into(),
            );
        }
    }
    let ValueRef::Text(bytes) = name else {
        return Err(invalid("native owner name is not text").into());
    };
    let text = std::str::from_utf8(bytes).map_err(|_| invalid("invalid owner name"))?;
    let _names = control
        .memory()
        .reserve(
            bytes
                .len()
                .checked_mul(4)
                .ok_or(VersionError::from(MemoryError::SizeOverflow))?,
        )
        .map_err(VersionError::from)?;
    if let Ok(relation) = uqa_storage::RelationIdentity::from_legacy_name(text) {
        if relation.qualified_name() == text {
            let omitted: Option<bool> = connection.query_row("SELECT NOT EXISTS(SELECT 1 FROM _uqa_mvcc_native_expected WHERE family = 41 AND physical_key = __uqa_mvcc_native_key(41, ?1, ?2) AND old_key IS NOT NULL) FROM _tables WHERE schema_name = ?1 AND relation_name = ?2", params![relation.schema, relation.name], |row| row.get(0)).optional()?;
            if omitted == Some(true) {
                return Err(invalid("native owner change omits its table definition").into());
            }
        }
    }
    Ok(())
}
