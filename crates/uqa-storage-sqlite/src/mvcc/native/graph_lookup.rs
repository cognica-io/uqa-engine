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
pub(super) const SCOPED_SOURCES: [Family; 3] = [
    Family::StandaloneGraphVertices,
    Family::StandaloneGraphEdges,
    Family::StandaloneGraphMembership,
];

fn lookup_family(source: Family) -> Family {
    if source.is_standalone_graph() {
        Family::StandaloneGraphLookups
    } else {
        Family::GraphLookups
    }
}

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
            Self::Column(index) => format!(
                "{image}.\"{}\"",
                family.layout().columns[index + usize::from(family.is_standalone_graph())]
            ),
            Self::Text(value) => format!("'{value}'"),
            Self::Zero => "0".into(),
        }
    }
}

fn projections(family: Family) -> &'static [[Part; 5]] {
    use Part::{Column as C, Text as T, Zero as Z};
    match family {
        Family::GraphVertices | Family::StandaloneGraphVertices => {
            &[[T("label"), C(1), Z, T("vertex"), C(0)]]
        }
        Family::GraphEdges | Family::StandaloneGraphEdges => &[
            [T("label"), C(3), Z, T("edge"), C(0)],
            [T("source"), T(""), C(1), T("edge"), C(0)],
            [T("target"), T(""), C(2), T("edge"), C(0)],
        ],
        Family::GraphMembership | Family::StandaloneGraphMembership => {
            &[[T("member"), C(2), Z, C(0), C(1)]]
        }
        Family::GraphPathIndexState => &[[T("path"), C(1), Z, C(0), Z]],
        _ => &[],
    }
}

pub(super) fn rows(
    family: Family,
    source: &[ValueRef<'_>],
    mut visit: impl FnMut(&[ValueRef<'_>]) -> PhysicalResult<()>,
) -> PhysicalResult<()> {
    let offset = usize::from(family.is_standalone_graph());
    let mut row = [ValueRef::Null; 6];
    if offset != 0 {
        row[0] = source[0];
    }
    for projection in projections(family) {
        row[offset..offset + 5]
            .copy_from_slice(&projection.map(|part| part.value(&source[offset..])));
        visit(&row[..offset + 5])?;
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
    let mut slot = 0;
    rows(family, source, |row| {
        result[slot] = Some(NativeRecord::encode(
            lookup_family(family),
            NativeRecordOwner::Database(database),
            row,
            control,
        )?);
        slot += 1;
        Ok(())
    })?;
    Ok(result)
}

pub(super) fn seed(connection: &Connection, control: &StorageReadControl) -> PhysicalResult<()> {
    seed_sources(connection, &SOURCES, control)?;
    seed_sources(connection, &SCOPED_SOURCES, control)
}

pub(super) fn seed_sources(
    connection: &Connection,
    sources: &[Family],
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    for &family in sources {
        physical::visit(connection, family.layout(), control, |source| {
            rows(family, source, |row| {
                physical::upsert(connection, lookup_family(family).layout(), row, control)
            })
        })?;
    }
    Ok(())
}

/// SQL triggers and prepared-record validation use the same selector definitions as history backfill.
pub(super) fn triggers() -> Vec<(String, String)> {
    let mut triggers = source_triggers(&SOURCES);
    triggers.extend(source_triggers(&SCOPED_SOURCES));
    triggers
}

pub(super) fn source_triggers(sources: &[Family]) -> Vec<(String, String)> {
    let mut result = Vec::new();
    for &family in sources {
        let lookup = lookup_family(family);
        let table = lookup.layout().table;
        for (slot, projection) in projections(family).iter().enumerate() {
            let image = |image| {
                let mut columns = Vec::new();
                if family.is_standalone_graph() {
                    columns.push(format!("{image}.\"scope\""));
                }
                columns.extend(projection.map(|part| part.sql(family, image)));
                columns
            };
            let old = image("OLD");
            let new = image("NEW");
            let changed = old
                .iter()
                .zip(&new)
                .map(|(old, new)| format!("{old} IS NOT {new}"))
                .collect::<Vec<_>>()
                .join(" OR ");
            let key = lookup
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
