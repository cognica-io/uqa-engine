//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Named graph entities, membership, and snapshot replacement.

use super::{
    decode_catalog_id, encode_catalog_id, params, Catalog, EdgeRow, GraphSnapshot, Result,
};

impl Catalog {
    /// Register the existence of a named graph in the catalog.
    /// Matches UQA behavior for `Catalog.save_named_graph`.
    pub fn save_named_graph(&self, name: &str) -> Result<()> {
        self.conn.with(|c| {
            c.execute(
                "INSERT OR IGNORE INTO _named_graphs (name) VALUES (?1)",
                params![name],
            )?;
            Ok(())
        })
    }

    /// Drop the named-graph registry row plus every membership entry
    /// that scopes a vertex or edge to this graph. Vertex / edge rows
    /// stay in `_graph_vertices` / `_graph_edges` until they go
    /// orphan; call [`Catalog::purge_orphan_graph_entities`] after to
    /// GC them. Matches UQA behavior for `Catalog.drop_named_graph` plus the
    /// orphan sweep that the engine performs on its behalf.
    pub fn drop_named_graph(&self, name: &str) -> Result<()> {
        self.conn.with(|c| {
            c.execute("DELETE FROM _named_graphs WHERE name = ?1", params![name])?;
            c.execute(
                "DELETE FROM _graph_membership WHERE graph_name = ?1",
                params![name],
            )?;
            Ok(())
        })
    }

    /// Sorted list of every persisted named graph.
    /// Matches UQA behavior for `Catalog.load_named_graphs`.
    pub fn load_named_graphs(&self) -> Result<Vec<String>> {
        self.conn.with(|c| {
            let mut stmt = c.prepare("SELECT name FROM _named_graphs ORDER BY name")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    /// Persist a vertex by global id. `properties_json` is the JSON
    /// encoding of the property map. Matches UQA behavior for
    /// `Catalog.save_vertex` extended with the `label` column the
    /// `SQLiteGraphStore` writes alongside it.
    pub fn save_vertex(&self, vertex_id: u64, label: &str, properties_json: &str) -> Result<()> {
        let vertex_id = encode_catalog_id("vertex", vertex_id)?;
        self.conn.with(|c| {
            c.execute(
                "INSERT OR REPLACE INTO _graph_vertices (vertex_id, label, properties_json) \
                 VALUES (?1, ?2, ?3)",
                params![vertex_id, label, properties_json],
            )?;
            Ok(())
        })
    }

    /// Delete a vertex by global id.
    pub fn delete_vertex(&self, vertex_id: u64) -> Result<()> {
        let vertex_id = encode_catalog_id("vertex", vertex_id)?;
        self.conn.with(|c| {
            c.execute(
                "DELETE FROM _graph_vertices WHERE vertex_id = ?1",
                params![vertex_id],
            )?;
            Ok(())
        })
    }

    /// Every vertex row sorted by id, returned as
    /// `(vertex_id, label, properties_json)` so the caller rebuilds
    /// the `Vertex` from the typed columns plus the JSON-encoded
    /// property map. Matches UQA behavior for `Catalog.load_vertices`.
    pub fn load_vertices(&self) -> Result<Vec<(u64, String, String)>> {
        self.conn.with(|c| {
            let mut stmt = c.prepare(
                "SELECT vertex_id, label, properties_json FROM _graph_vertices ORDER BY vertex_id",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?;
            let mut out = Vec::new();
            for row in rows {
                let (id, label, props) = row?;
                out.push((decode_catalog_id("vertex", id)?, label, props));
            }
            Ok(out)
        })
    }

    /// Persist an edge by global id with its source / target vertices,
    /// label, and JSON-encoded property map. Matches UQA behavior for
    /// `Catalog.save_edge`.
    pub fn save_edge(
        &self,
        edge_id: u64,
        source_id: u64,
        target_id: u64,
        label: &str,
        properties_json: &str,
    ) -> Result<()> {
        let edge_id = encode_catalog_id("edge", edge_id)?;
        let source_id = encode_catalog_id("edge source vertex", source_id)?;
        let target_id = encode_catalog_id("edge target vertex", target_id)?;
        self.conn.with(|c| {
            c.execute(
                "INSERT OR REPLACE INTO _graph_edges \
                    (edge_id, source_id, target_id, label, properties_json) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![edge_id, source_id, target_id, label, properties_json],
            )?;
            Ok(())
        })
    }

    /// Delete an edge by global id.
    pub fn delete_edge(&self, edge_id: u64) -> Result<()> {
        let edge_id = encode_catalog_id("edge", edge_id)?;
        self.conn.with(|c| {
            c.execute(
                "DELETE FROM _graph_edges WHERE edge_id = ?1",
                params![edge_id],
            )?;
            Ok(())
        })
    }

    /// Every edge row sorted by id. Matches UQA behavior for `Catalog.load_edges`.
    pub fn load_edges(&self) -> Result<Vec<EdgeRow>> {
        self.conn.with(|c| {
            let mut stmt = c.prepare(
                "SELECT edge_id, source_id, target_id, label, properties_json \
                   FROM _graph_edges ORDER BY edge_id",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                ))
            })?;
            let mut out = Vec::new();
            for row in rows {
                let (id, src, tgt, label, props) = row?;
                out.push(EdgeRow {
                    edge_id: decode_catalog_id("edge", id)?,
                    source_id: decode_catalog_id("edge source vertex", src)?,
                    target_id: decode_catalog_id("edge target vertex", tgt)?,
                    label,
                    properties_json: props,
                });
            }
            Ok(out)
        })
    }

    /// Attach `entity_id` (a vertex when `entity_type == "vertex"`, an
    /// edge when `"edge"`) to `graph_name`. The same entity can sit in
    /// many graphs; the row is keyed by the full triple so duplicate
    /// attaches no-op.
    pub fn save_graph_membership(
        &self,
        entity_type: &str,
        entity_id: u64,
        graph_name: &str,
    ) -> Result<()> {
        let entity_id = encode_catalog_id("graph membership entity", entity_id)?;
        self.conn.with(|c| {
            c.execute(
                "INSERT OR IGNORE INTO _graph_membership \
                    (entity_type, entity_id, graph_name) \
                 VALUES (?1, ?2, ?3)",
                params![entity_type, entity_id, graph_name],
            )?;
            Ok(())
        })
    }

    /// Detach `entity_id` from `graph_name`.
    pub fn delete_graph_membership(
        &self,
        entity_type: &str,
        entity_id: u64,
        graph_name: &str,
    ) -> Result<()> {
        let entity_id = encode_catalog_id("graph membership entity", entity_id)?;
        self.conn.with(|c| {
            c.execute(
                "DELETE FROM _graph_membership \
                  WHERE entity_type = ?1 AND entity_id = ?2 AND graph_name = ?3",
                params![entity_type, entity_id, graph_name],
            )?;
            Ok(())
        })
    }

    /// Detach every entity from `graph_name`. Used as the prelude to a
    /// full graph drop / Cypher resync.
    pub fn delete_graph_membership_for_graph(&self, graph_name: &str) -> Result<()> {
        self.conn.with(|c| {
            c.execute(
                "DELETE FROM _graph_membership WHERE graph_name = ?1",
                params![graph_name],
            )?;
            Ok(())
        })
    }

    /// Every membership row, returned as `(entity_type, entity_id, graph_name)`.
    pub fn load_graph_memberships(&self) -> Result<Vec<(String, u64, String)>> {
        self.conn.with(|c| {
            let mut stmt = c.prepare(
                "SELECT entity_type, entity_id, graph_name FROM _graph_membership \
                  ORDER BY graph_name, entity_type, entity_id",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?;
            let mut out = Vec::new();
            for row in rows {
                let (ty, id, graph) = row?;
                out.push((ty, decode_catalog_id("graph membership entity", id)?, graph));
            }
            Ok(out)
        })
    }

    /// Drop vertex / edge rows that no membership row still references.
    /// Run after a detach / drop to garbage-collect orphaned entities.
    pub fn purge_orphan_graph_entities(&self) -> Result<()> {
        self.conn.with(|c| {
            c.execute(
                "DELETE FROM _graph_vertices \
                  WHERE vertex_id NOT IN ( \
                    SELECT entity_id FROM _graph_membership WHERE entity_type = 'vertex' \
                  )",
                [],
            )?;
            c.execute(
                "DELETE FROM _graph_edges \
                  WHERE edge_id NOT IN ( \
                    SELECT entity_id FROM _graph_membership WHERE entity_type = 'edge' \
                  )",
                [],
            )?;
            Ok(())
        })
    }

    pub fn replace_named_graph(&self, graph_name: &str, snapshot: &GraphSnapshot) -> Result<()> {
        self.conn.with_mut(|c| {
            let tx = c.savepoint()?;
            tx.execute(
                "INSERT OR IGNORE INTO _named_graphs (name) VALUES (?1)",
                params![graph_name],
            )?;
            tx.execute(
                "DELETE FROM _graph_membership WHERE graph_name = ?1",
                params![graph_name],
            )?;
            tx.execute(
                "DELETE FROM _path_indexes
                  WHERE substr(graph_name, 1, length(?1) + 2) = ?1 || '::'",
                params![graph_name],
            )?;
            for vertex in &snapshot.vertices {
                let vertex_id = encode_catalog_id("vertex", vertex.vertex_id)?;
                tx.execute(
                    "INSERT OR REPLACE INTO _graph_vertices
                        (vertex_id, label, properties_json) VALUES (?1, ?2, ?3)",
                    params![vertex_id, vertex.label, vertex.properties_json],
                )?;
                tx.execute(
                    "INSERT OR IGNORE INTO _graph_membership
                        (entity_type, entity_id, graph_name) VALUES ('vertex', ?1, ?2)",
                    params![vertex_id, graph_name],
                )?;
            }
            for edge in &snapshot.edges {
                let edge_id = encode_catalog_id("edge", edge.edge_id)?;
                let source_id = encode_catalog_id("edge source vertex", edge.source_id)?;
                let target_id = encode_catalog_id("edge target vertex", edge.target_id)?;
                tx.execute(
                    "INSERT OR REPLACE INTO _graph_edges
                        (edge_id, source_id, target_id, label, properties_json)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        edge_id,
                        source_id,
                        target_id,
                        edge.label,
                        edge.properties_json
                    ],
                )?;
                tx.execute(
                    "INSERT OR IGNORE INTO _graph_membership
                        (entity_type, entity_id, graph_name) VALUES ('edge', ?1, ?2)",
                    params![edge_id, graph_name],
                )?;
            }
            tx.execute(
                "INSERT OR REPLACE INTO _metadata (key, value) VALUES (?1, ?2)",
                params![
                    format!("graph_label_registry::{graph_name}"),
                    snapshot.label_registry_json
                ],
            )?;
            Self::purge_orphan_graph_entities_on(&tx)?;
            tx.commit()?;
            Ok(())
        })
    }

    pub fn drop_named_graph_data(&self, graph_name: &str) -> Result<()> {
        self.conn.with_mut(|c| {
            let tx = c.savepoint()?;
            tx.execute(
                "DELETE FROM _named_graphs WHERE name = ?1",
                params![graph_name],
            )?;
            tx.execute(
                "DELETE FROM _graph_membership WHERE graph_name = ?1",
                params![graph_name],
            )?;
            tx.execute(
                "DELETE FROM _metadata WHERE key = ?1",
                params![format!("graph_label_registry::{graph_name}")],
            )?;
            tx.execute(
                "DELETE FROM _path_indexes
                  WHERE substr(graph_name, 1, length(?1) + 2) = ?1 || '::'",
                params![graph_name],
            )?;
            Self::purge_orphan_graph_entities_on(&tx)?;
            tx.commit()?;
            Ok(())
        })
    }

    pub(super) fn purge_orphan_graph_entities_on(c: &rusqlite::Connection) -> Result<()> {
        c.execute(
            "DELETE FROM _graph_vertices
              WHERE vertex_id NOT IN (
                SELECT entity_id FROM _graph_membership WHERE entity_type = 'vertex'
              )",
            [],
        )?;
        c.execute(
            "DELETE FROM _graph_edges
              WHERE edge_id NOT IN (
                SELECT entity_id FROM _graph_membership WHERE entity_type = 'edge'
              )",
            [],
        )?;
        Ok(())
    }
}
