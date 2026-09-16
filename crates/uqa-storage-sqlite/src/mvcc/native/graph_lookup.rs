//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Versioned graph label, adjacency and graph-membership selectors contain identities, never entity properties.

mod migration;
mod validation;

use rusqlite::{types::ValueRef, Connection};
use uqa_storage::{mvcc::DatabaseId, read_control::StorageReadControl};

use super::{physical, NativeRecord, NativeRecordFamily as Family, NativeRecordOwner};
use crate::mvcc::PhysicalResult;

pub(super) use migration::backfill;
pub(super) use validation::{validate_deletions, validate_row};

pub(super) const SQL: &str = "CREATE TABLE _uqa_mvcc_native_graph_lookup (kind TEXT NOT NULL, text_key TEXT NOT NULL, integer_key INTEGER NOT NULL, entity_type TEXT NOT NULL, entity_id INTEGER NOT NULL, PRIMARY KEY(kind, text_key, integer_key, entity_type, entity_id)) WITHOUT ROWID";
pub(super) const SOURCES: [Family; 4] = [
    Family::GraphVertices,
    Family::GraphEdges,
    Family::GraphMembership,
    Family::GraphPathIndexState,
];

#[derive(Clone, Copy)]
enum Part {
    Column(usize),
    Text(&'static str),
    Zero,
}

impl Part {
    fn value<'a>(self, source: &[ValueRef<'a>]) -> ValueRef<'a> {
        match self {
            Self::Column(index) => source[index],
            Self::Text(value) => ValueRef::Text(value.as_bytes()),
            Self::Zero => ValueRef::Integer(0),
        }
    }

    fn sql(self, family: Family, image: &str) -> String {
        match self {
            Self::Column(index) => format!("{image}.\"{}\"", family.layout().columns[index]),
            Self::Text(value) => format!("'{value}'"),
            Self::Zero => "0".into(),
        }
    }
}

fn projections(family: Family) -> &'static [[Part; 5]] {
    use Part::{Column as C, Text as T, Zero as Z};
    match family {
        Family::GraphVertices => &[[T("label"), C(1), Z, T("vertex"), C(0)]],
        Family::GraphEdges => &[
            [T("label"), C(3), Z, T("edge"), C(0)],
            [T("source"), T(""), C(1), T("edge"), C(0)],
            [T("target"), T(""), C(2), T("edge"), C(0)],
        ],
        Family::GraphMembership => &[[T("member"), C(2), Z, C(0), C(1)]],
        Family::GraphPathIndexState => &[[T("path"), C(1), Z, C(0), Z]],
        _ => &[],
    }
}

pub(super) fn rows(
    family: Family,
    source: &[ValueRef<'_>],
    mut visit: impl FnMut(&[ValueRef<'_>]) -> PhysicalResult<()>,
) -> PhysicalResult<()> {
    for projection in projections(family) {
        visit(&projection.map(|part| part.value(source)))?;
    }
    Ok(())
}

pub(super) fn records(
    database: DatabaseId,
    family: Family,
    source: &[ValueRef<'_>],
    control: &StorageReadControl,
) -> PhysicalResult<[Option<NativeRecord>; 3]> {
    let mut result = [None, None, None];
    for (slot, projection) in projections(family).iter().enumerate() {
        result[slot] = Some(NativeRecord::encode(
            Family::GraphLookups,
            NativeRecordOwner::Database(database),
            &projection.map(|part| part.value(source)),
            control,
        )?);
    }
    Ok(result)
}

pub(super) fn seed(connection: &Connection, control: &StorageReadControl) -> PhysicalResult<()> {
    seed_sources(connection, &SOURCES, control)
}

pub(super) fn seed_sources(
    connection: &Connection,
    sources: &[Family],
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    for &family in sources {
        physical::visit(connection, family.layout(), control, |source| {
            rows(family, source, |row| {
                physical::upsert(connection, Family::GraphLookups.layout(), row, control)
            })
        })?;
    }
    Ok(())
}

/// SQL triggers and prepared-record validation use the same selector definitions as history backfill.
pub(super) fn triggers() -> Vec<(String, String)> {
    source_triggers(&SOURCES)
}

pub(super) fn source_triggers(sources: &[Family]) -> Vec<(String, String)> {
    let mut result = Vec::new();
    let table = Family::GraphLookups.layout().table;
    for &family in sources {
        for (slot, projection) in projections(family).iter().enumerate() {
            let old = projection.map(|part| part.sql(family, "OLD"));
            let new = projection.map(|part| part.sql(family, "NEW"));
            let changed = old
                .iter()
                .zip(&new)
                .map(|(old, new)| format!("{old} IS NOT {new}"))
                .collect::<Vec<_>>()
                .join(" OR ");
            let key = Family::GraphLookups
                .layout()
                .columns
                .iter()
                .zip(&old)
                .map(|(column, old)| format!("\"{column}\" = {old}"))
                .collect::<Vec<_>>()
                .join(" AND ");
            let insert = format!("INSERT OR IGNORE INTO {table} VALUES ({});", new.join(", "));
            let delete = format!("DELETE FROM {table} WHERE {key};");
            for action in ["INSERT", "UPDATE", "DELETE"] {
                let name = format!("_uqa_mvcc_graph_lookup_{}_{slot}_{action}", family.id());
                let condition = if action == "UPDATE" {
                    format!(" WHEN {changed}")
                } else {
                    String::new()
                };
                let body = match action {
                    "INSERT" => insert.clone(),
                    "DELETE" => delete.clone(),
                    _ => format!("{delete} {insert}"),
                };
                result.push((
                    name.clone(),
                    format!(
                        "CREATE TRIGGER {name} AFTER {action} ON {}{condition} BEGIN {body} END",
                        family.layout().table
                    ),
                ));
            }
        }
    }
    result
}
