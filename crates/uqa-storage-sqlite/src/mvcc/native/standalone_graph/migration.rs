//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Move recognized standalone graph families into scoped rows before native baseline capture.

use rusqlite::{params, types::ValueRef, Connection, OptionalExtension};
use uqa_core::memory::{BudgetedVec, MemoryError};
use uqa_storage::{mvcc::VersionError, read_control::StorageReadControl};

use crate::mvcc::{
    native::{invalid, NativeRecordFamily as Family},
    schema, PhysicalResult,
};

struct Source {
    family: Family,
    name: &'static str,
    columns: &'static [(&'static str, &'static str, usize)],
    optional: Option<&'static str>,
}

const SOURCES: &[Source] = &[
    Source {
        family: Family::StandaloneGraphCatalog,
        name: "_graph_catalog",
        columns: &[("name", "TEXT", 1)],
        optional: Some("registry_json"),
    },
    Source {
        family: Family::StandaloneGraphMetadata,
        name: "_graph_metadata",
        columns: &[("key", "TEXT", 1), ("value", "TEXT", 0)],
        optional: None,
    },
    Source {
        family: Family::StandaloneGraphVertices,
        name: "_graph_vertices",
        columns: &[
            ("vertex_id", "INTEGER", 1),
            ("label", "TEXT", 0),
            ("properties_json", "TEXT", 0),
        ],
        optional: Some("properties_format"),
    },
    Source {
        family: Family::StandaloneGraphEdges,
        name: "_graph_edges",
        columns: &[
            ("edge_id", "INTEGER", 1),
            ("source_id", "INTEGER", 0),
            ("target_id", "INTEGER", 0),
            ("label", "TEXT", 0),
            ("properties_json", "TEXT", 0),
        ],
        optional: Some("properties_format"),
    },
    Source {
        family: Family::StandaloneGraphMembership,
        name: "_graph_membership",
        columns: &[
            ("graph", "TEXT", 1),
            ("entity_kind", "TEXT", 2),
            ("entity_id", "INTEGER", 3),
        ],
        optional: None,
    },
];

fn scope(suffix: &str) -> PhysicalResult<String> {
    let value = if suffix.is_empty() {
        ""
    } else {
        suffix
            .strip_prefix('_')
            .filter(|value| !value.is_empty())
            .ok_or_else(|| invalid("invalid standalone graph source suffix"))?
    };
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return Err(invalid("invalid standalone graph source suffix").into());
    }
    Ok(value.to_ascii_lowercase())
}

fn source_shape(
    connection: &Connection,
    name: &str,
    source: &Source,
) -> PhysicalResult<Option<bool>> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info(\"{name}\")"))?;
    let mut rows = statement.query([])?;
    let mut count = 0;
    let mut optional = false;
    while let Some(row) = rows.next()? {
        let name = row
            .get_ref(1)?
            .as_str()
            .map_err(|_| invalid("invalid standalone graph column name"))?;
        let kind = row
            .get_ref(2)?
            .as_str()
            .map_err(|_| invalid("invalid standalone graph column type"))?;
        let primary: i64 = row.get(5)?;
        if let Some(&(expected, declaration, key)) = source.columns.get(count) {
            if !name.eq_ignore_ascii_case(expected)
                || !kind.eq_ignore_ascii_case(declaration)
                || usize::try_from(primary) != Ok(key)
            {
                return Err(
                    invalid("standalone graph source columns do not match their format").into(),
                );
            }
        } else if count == source.columns.len()
            && source
                .optional
                .is_some_and(|expected| name.eq_ignore_ascii_case(expected))
        {
            let declaration = if source.optional == Some("registry_json") {
                "TEXT"
            } else {
                "INTEGER"
            };
            if !kind.eq_ignore_ascii_case(declaration) || primary != 0 {
                return Err(invalid("invalid optional standalone graph column").into());
            }
            optional = true;
        } else {
            return Err(invalid("unknown standalone graph source column").into());
        }
        count += 1;
    }
    if count == 0 {
        return Ok(None);
    }
    if count < source.columns.len() {
        return Err(invalid("missing standalone graph source column").into());
    }
    Ok(Some(optional))
}

fn import_scope(
    connection: &Connection,
    suffix: &str,
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    let scratch = suffix
        .len()
        .checked_mul(16)
        .and_then(|size| size.checked_add(4096))
        .ok_or(MemoryError::SizeOverflow)
        .map_err(VersionError::from)?;
    let _memory = control
        .memory()
        .reserve(scratch)
        .map_err(VersionError::from)?;
    let scope = scope(suffix)?;
    connection.execute(
        "INSERT INTO _uqa_mvcc_native_standalone_graph_scopes (scope,legacy_suffix) VALUES (?1,?2)",
        params![scope, suffix],
    )?;
    for source in SOURCES {
        control.cancellation().check().map_err(VersionError::from)?;
        let table = format!("{}{suffix}", source.name);
        let Some(optional) = source_shape(connection, &table, source)? else {
            if source.family == Family::StandaloneGraphMetadata {
                continue;
            }
            return Err(invalid("incomplete standalone graph table family").into());
        };
        let projection = match source.family {
            Family::StandaloneGraphCatalog if optional => "name, registry_json",
            Family::StandaloneGraphCatalog => "name, '{}'",
            Family::StandaloneGraphMetadata => "key, value",
            Family::StandaloneGraphVertices if optional => "vertex_id, label, properties_json, properties_format",
            Family::StandaloneGraphVertices => "vertex_id, label, properties_json, 1",
            Family::StandaloneGraphEdges if optional => "edge_id, source_id, target_id, label, properties_json, properties_format",
            Family::StandaloneGraphEdges => "edge_id, source_id, target_id, label, properties_json, 1",
            Family::StandaloneGraphMembership => "CASE entity_kind WHEN 'v' THEN 'vertex' WHEN 'e' THEN 'edge' ELSE entity_kind END, entity_id, graph",
            _ => unreachable!("standalone graph source family"),
        };
        connection.execute(
            &format!(
                "INSERT INTO {} SELECT ?1, {projection} FROM \"{table}\"",
                source.family.layout().table
            ),
            [&scope],
        )?;
        connection.execute_batch(&format!("DROP TABLE \"{table}\""))?;
    }
    Ok(())
}

pub(in crate::mvcc::native) fn import_sources(
    connection: &Connection,
    control: &StorageReadControl,
) -> PhysicalResult<bool> {
    super::create(connection)?;
    let mut imported = false;
    loop {
        control.cancellation().check().map_err(VersionError::from)?;
        let mut suffix = BudgetedVec::new(control.memory());
        // Finish the schema read before replacing its tables with scoped records.
        let found = {
            let mut statement = connection.prepare("SELECT name FROM sqlite_schema WHERE type = 'table' AND (lower(name) = '_graph_catalog' OR lower(name) GLOB '_graph_catalog_*') ORDER BY name LIMIT 1")?;
            let mut rows = statement.query([])?;
            if let Some(row) = rows.next()? {
                let name = row
                    .get_ref(0)?
                    .as_str()
                    .map_err(|_| invalid("invalid standalone graph table name"))?;
                suffix
                    .extend_from_slice(&name.as_bytes()["_graph_catalog".len()..])
                    .map_err(VersionError::from)?;
                true
            } else {
                false
            }
        };
        if !found {
            return Ok(imported);
        }
        import_scope(
            connection,
            std::str::from_utf8(&suffix).map_err(|_| invalid("invalid standalone graph suffix"))?,
            control,
        )?;
        imported = true;
    }
}

fn guard_sql(source: &Source, suffix: &str, scope: &str) -> Option<(String, String)> {
    if suffix.is_empty()
        && matches!(
            source.family,
            Family::StandaloneGraphVertices
                | Family::StandaloneGraphEdges
                | Family::StandaloneGraphMembership
        )
    {
        // The native catalog reuses these names; its versioned table guards reject legacy writes.
        return None;
    }
    let name = format!("{}{suffix}", source.name);
    let columns = match source.family {
        Family::StandaloneGraphCatalog => "name,registry_json",
        Family::StandaloneGraphMetadata => "key,value",
        Family::StandaloneGraphVertices => "vertex_id,label,properties_json,properties_format",
        Family::StandaloneGraphEdges => "edge_id,source_id,target_id,label,properties_json,properties_format",
        Family::StandaloneGraphMembership => "graph_name AS graph,CASE entity_type WHEN 'vertex' THEN 'v' ELSE 'e' END AS entity_kind,entity_id",
        _ => unreachable!("standalone graph source family"),
    };
    let sql = format!(
        "CREATE VIEW \"{name}\" AS SELECT {columns} FROM {} WHERE scope = '{scope}'",
        source.family.layout().table
    );
    Some((name, sql))
}

fn retained_suffix(
    name: &str,
    legacy: ValueRef<'_>,
    control: &StorageReadControl,
) -> PhysicalResult<BudgetedVec<u8>> {
    let mut suffix = BudgetedVec::new(control.memory());
    if legacy == ValueRef::Null {
        if !name.is_empty() {
            suffix.push(b'_').map_err(VersionError::from)?;
        }
        suffix
            .extend_from_slice(name.as_bytes())
            .map_err(VersionError::from)?;
    } else {
        let legacy = legacy
            .as_str()
            .map_err(|_| invalid("invalid standalone graph legacy suffix"))?;
        suffix
            .extend_from_slice(legacy.as_bytes())
            .map_err(VersionError::from)?;
    }
    Ok(suffix)
}

fn next_scope(
    connection: &Connection,
    after: Option<&[u8]>,
    control: &StorageReadControl,
) -> PhysicalResult<Option<(BudgetedVec<u8>, BudgetedVec<u8>)>> {
    control.cancellation().check().map_err(VersionError::from)?;
    let after = after.map(|bytes| std::str::from_utf8(bytes).expect("retained UTF-8 scope"));
    connection.query_row("SELECT scope,legacy_suffix FROM _uqa_mvcc_native_standalone_graph_scopes WHERE (?1 IS NULL OR scope > ?1) ORDER BY scope LIMIT 1", [after], |row| {
        let retain = || -> PhysicalResult<_> {
            let name = row.get_ref(0)?.as_str().map_err(|_| invalid("invalid standalone graph scope"))?;
            let mut retained = BudgetedVec::new(control.memory());
            retained.extend_from_slice(name.as_bytes()).map_err(VersionError::from)?;
            Ok((retained, retained_suffix(name,row.get_ref(1)?,control)?))
        };
        Ok(retain())
    }).optional()?.transpose()
}

fn scope_guards(
    connection: &Connection,
    name: &str,
    suffix: &str,
    control: &StorageReadControl,
    install: bool,
) -> PhysicalResult<()> {
    let size = name
        .len()
        .checked_add(suffix.len())
        .and_then(|size| size.checked_mul(16))
        .and_then(|size| size.checked_add(4096))
        .ok_or(MemoryError::SizeOverflow)
        .map_err(VersionError::from)?;
    let _scratch = control.memory().reserve(size).map_err(VersionError::from)?;
    if scope(suffix)? != name {
        return Err(invalid("standalone graph source scope changed").into());
    }
    for source in SOURCES {
        control.cancellation().check().map_err(VersionError::from)?;
        if let Some((view, sql)) = guard_sql(source, suffix, name) {
            if install {
                connection.execute_batch(&sql)?;
            } else if schema::definition_matches(connection, &view, &sql)? != Some(true) {
                return Err(invalid("missing or changed standalone graph legacy guard").into());
            }
        }
    }
    Ok(())
}

pub(in crate::mvcc::native) fn install_scope_guards(
    connection: &Connection,
    values: &[ValueRef<'_>],
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    let name = values[0]
        .as_str()
        .map_err(|_| invalid("invalid standalone graph scope"))?;
    let suffix = retained_suffix(name, values[1], control)?;
    scope_guards(
        connection,
        name,
        std::str::from_utf8(&suffix).expect("retained UTF-8 suffix"),
        control,
        true,
    )
}

fn legacy_guards(
    connection: &Connection,
    control: &StorageReadControl,
    install: bool,
) -> PhysicalResult<()> {
    let mut after: Option<BudgetedVec<u8>> = None;
    loop {
        let Some((name, suffix)) = next_scope(connection, after.as_deref(), control)? else {
            return Ok(());
        };
        scope_guards(
            connection,
            std::str::from_utf8(&name).expect("retained UTF-8 scope"),
            std::str::from_utf8(&suffix).expect("retained UTF-8 suffix"),
            control,
            install,
        )?;
        after = Some(name);
    }
}

pub(in crate::mvcc::native) fn install_legacy_guards(
    connection: &Connection,
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    legacy_guards(connection, control, true)
}

pub(in crate::mvcc::native) fn validate_legacy_guards(
    connection: &Connection,
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    legacy_guards(connection, control, false)
}
