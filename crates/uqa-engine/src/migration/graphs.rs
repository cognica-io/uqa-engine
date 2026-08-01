//! Named graph, vertex, edge, and membership restoration.

use super::{
    json_object_to_value_map, table_columns, table_exists, BTreeMap, BTreeSet, Connection, Edge,
    Engine, PythonMigrationError, PythonMigrationReport, Vertex,
};

pub(super) fn migrate_graphs(
    conn: &Connection,
    engine: &Engine,
    report: &mut PythonMigrationReport,
) -> Result<(), PythonMigrationError> {
    let graph_names = load_graph_names(conn)?;
    for graph in &graph_names {
        engine.create_graph(graph)?;
    }
    report.graphs = graph_names.len();

    let vertices = load_vertices(conn)?;
    let edges = load_edges(conn)?;
    let memberships = load_graph_memberships(conn)?;

    let mut vertex_memberships = Vec::new();
    let mut edge_memberships = Vec::new();
    for (entity_type, entity_id, graph_name) in memberships {
        match entity_type.as_str() {
            "vertex" => vertex_memberships.push((entity_id, graph_name)),
            "edge" => edge_memberships.push((entity_id, graph_name)),
            other => {
                return Err(PythonMigrationError::Invalid(format!(
                    "unknown graph membership entity type `{other}`"
                )))
            }
        }
    }
    // Install vertices first so edge endpoint validation sees a complete graph
    // membership, regardless of the source catalog's row order.
    for (entity_id, graph_name) in vertex_memberships {
        let vertex = vertices.get(&entity_id).ok_or_else(|| {
            PythonMigrationError::Invalid(format!(
                "graph membership references missing vertex {entity_id} in `{graph_name}`"
            ))
        })?;
        engine.add_graph_vertex(vertex.clone(), &graph_name)?;
    }
    for (entity_id, graph_name) in edge_memberships {
        let edge = edges.get(&entity_id).ok_or_else(|| {
            PythonMigrationError::Invalid(format!(
                "graph membership references missing edge {entity_id} in `{graph_name}`"
            ))
        })?;
        engine.add_graph_edge(edge.clone(), &graph_name)?;
    }
    report.graph_vertices = vertices.len();
    report.graph_edges = edges.len();
    Ok(())
}

pub(super) fn load_graph_names(conn: &Connection) -> Result<Vec<String>, PythonMigrationError> {
    let mut names = BTreeSet::new();
    for (table, column) in [("_named_graphs", "name"), ("_graph_catalog", "graph_name")] {
        if !table_exists(conn, table)? {
            continue;
        }
        let sql = format!("SELECT {column} FROM {table}");
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        for row in rows {
            names.insert(row?);
        }
    }
    Ok(names.into_iter().collect())
}

pub(super) fn load_vertices(
    conn: &Connection,
) -> Result<BTreeMap<u64, Vertex>, PythonMigrationError> {
    if !table_exists(conn, "_graph_vertices")? {
        return Ok(BTreeMap::new());
    }
    let cols = table_columns(conn, "_graph_vertices")?;
    let has_label = cols.iter().any(|col| col == "label");
    let sql = if has_label {
        "SELECT vertex_id, label, properties_json FROM _graph_vertices ORDER BY vertex_id"
            .to_string()
    } else {
        "SELECT vertex_id, '' AS label, properties_json FROM _graph_vertices ORDER BY vertex_id"
            .to_string()
    };
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut out = BTreeMap::new();
    for row in rows {
        let (id, label, props_json) = row?;
        let vertex_id = u64::try_from(id)
            .map_err(|_| PythonMigrationError::Invalid(format!("negative graph vertex id {id}")))?;
        let props = json_object_to_value_map(&props_json)?;
        out.insert(
            vertex_id,
            Vertex {
                vertex_id,
                label,
                properties: props,
            },
        );
    }
    Ok(out)
}

pub(super) fn load_edges(conn: &Connection) -> Result<BTreeMap<u64, Edge>, PythonMigrationError> {
    if !table_exists(conn, "_graph_edges")? {
        return Ok(BTreeMap::new());
    }
    let mut stmt = conn.prepare(
        "SELECT edge_id, source_id, target_id, label, properties_json
           FROM _graph_edges
          ORDER BY edge_id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
        ))
    })?;
    let mut out = BTreeMap::new();
    for row in rows {
        let (id, source_id, target_id, label, props_json) = row?;
        let edge_id = u64::try_from(id)
            .map_err(|_| PythonMigrationError::Invalid(format!("negative graph edge id {id}")))?;
        let source_id = u64::try_from(source_id).map_err(|_| {
            PythonMigrationError::Invalid(format!(
                "edge {edge_id} has negative source vertex id {source_id}"
            ))
        })?;
        let target_id = u64::try_from(target_id).map_err(|_| {
            PythonMigrationError::Invalid(format!(
                "edge {edge_id} has negative target vertex id {target_id}"
            ))
        })?;
        let props = json_object_to_value_map(&props_json)?;
        out.insert(
            edge_id,
            Edge {
                edge_id,
                source_id,
                target_id,
                label,
                properties: props,
            },
        );
    }
    Ok(out)
}

pub(super) fn load_graph_memberships(
    conn: &Connection,
) -> Result<Vec<(String, u64, String)>, PythonMigrationError> {
    if !table_exists(conn, "_graph_membership")? {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT entity_type, entity_id, graph_name
           FROM _graph_membership
          ORDER BY graph_name, entity_type, entity_id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (ty, id, graph) = row?;
        let id = u64::try_from(id).map_err(|_| {
            PythonMigrationError::Invalid(format!(
                "graph membership `{ty}` in `{graph}` has negative entity id {id}"
            ))
        })?;
        out.push((ty, id, graph));
    }
    Ok(out)
}
