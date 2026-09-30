//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Apache AGE graph and label OIDs, drawn from the database's counter in the order AGE 1.8 creates the objects: `create_graph` creates the graph's schema and label id sequence and then the default vertex and edge labels, and `create_vlabel`, `create_elabel` and a Cypher write that names a new label create a label that inherits from its kind's default label.

use std::collections::BTreeMap;

use uqa_sql::catalog::graph_oids::{
    EndpointIndexOids, GraphCatalogOids, IndexConstraintOids, LabelCatalogOids,
};
use uqa_sql::catalog::relation_oids::RelationOidKind;
use uqa_sql::schema::constraint_metadata::CatalogOidClass;
use uqa_sql::SQLError;

use super::ReservedCatalogIdentityAllocator;

/// AGE's label ids of the default vertex and edge labels.
const DEFAULT_VERTEX_LABEL_ID: u32 = 1;
const DEFAULT_EDGE_LABEL_ID: u32 = 2;

/// Whether a label holds vertices or edges, and whether it is one of the two labels every graph starts with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LabelShape {
    pub edge: bool,
    pub default: bool,
}

impl ReservedCatalogIdentityAllocator<'_> {
    /// `create_graph`: the schema, whose OID becomes the graph's id, the label id sequence, and the default vertex and edge labels. `namespace_in_use` tells whether a schema already holds an OID.
    pub fn allocate_graph_oids(
        &mut self,
        namespace_in_use: impl FnMut(i64) -> Result<bool, SQLError>,
    ) -> Result<GraphCatalogOids, SQLError> {
        let namespace = self.allocate_namespace_oid(namespace_in_use)?;
        let label_sequence = self
            .allocate_relation_oids(RelationOidKind::Sequence)?
            .relation;
        let mut labels = BTreeMap::new();
        for (id, edge) in [
            (DEFAULT_VERTEX_LABEL_ID, false),
            (DEFAULT_EDGE_LABEL_ID, true),
        ] {
            labels.insert(
                id,
                self.allocate_label_oids(LabelShape {
                    edge,
                    default: true,
                })?,
            );
        }
        Ok(GraphCatalogOids {
            namespace,
            label_sequence,
            labels,
        })
    }

    /// One label's objects. A default label's table is created with its columns, defaults and constraints; any other label's table inherits its kind's default label, whose `id` default it copies first and then replaces with one on its own sequence, so its inherited NOT NULL constraints arrive in name order and its `id` default is allocated last.
    pub fn allocate_label_oids(&mut self, shape: LabelShape) -> Result<LabelCatalogOids, SQLError> {
        let columns: &[&str] = if shape.edge {
            &["id", "start_id", "end_id", "properties"]
        } else {
            &["id", "properties"]
        };
        let sequence = self
            .allocate_relation_oids(RelationOidKind::Sequence)?
            .relation;
        let relation = self.allocate_relation_oids(RelationOidKind::Table)?;
        let (id_default, properties_default, not_null) = if shape.default {
            let id_default = self.allocate(CatalogOidClass::AttributeDefault)?;
            let properties_default = self.allocate(CatalogOidClass::AttributeDefault)?;
            let mut not_null = BTreeMap::new();
            for column in columns {
                not_null.insert(
                    (*column).to_string(),
                    self.allocate(CatalogOidClass::Constraint)?,
                );
            }
            (Some(id_default), properties_default, not_null)
        } else {
            // The default copied from the parent's `id` column, which the label's own sequence replaces.
            self.allocate(CatalogOidClass::AttributeDefault)?;
            let properties_default = self.allocate(CatalogOidClass::AttributeDefault)?;
            let mut names = columns.to_vec();
            names.sort_unstable();
            let mut not_null = BTreeMap::new();
            for column in names {
                not_null.insert(
                    column.to_string(),
                    self.allocate(CatalogOidClass::Constraint)?,
                );
            }
            (None, properties_default, not_null)
        };
        let toast_table = self.allocate(CatalogOidClass::Relation)?;
        let toast_index = self.allocate(CatalogOidClass::Relation)?;
        let primary_key = if !shape.edge || shape.default {
            Some(IndexConstraintOids {
                index: self.allocate(CatalogOidClass::Relation)?,
                constraint: self.allocate(CatalogOidClass::Constraint)?,
            })
        } else {
            None
        };
        let endpoint_indexes = if shape.edge {
            Some(EndpointIndexOids {
                start_id: self.allocate(CatalogOidClass::Relation)?,
                end_id: self.allocate(CatalogOidClass::Relation)?,
            })
        } else {
            None
        };
        let trigger = self.allocate(CatalogOidClass::Trigger)?;
        let id_default = match id_default {
            Some(id_default) => id_default,
            None => self.allocate(CatalogOidClass::AttributeDefault)?,
        };
        Ok(LabelCatalogOids {
            sequence,
            relation,
            id_default,
            properties_default,
            not_null,
            toast_table,
            toast_index,
            primary_key,
            endpoint_indexes,
            trigger,
        })
    }

    fn allocate(&mut self, class: CatalogOidClass) -> Result<u32, SQLError> {
        use uqa_sql::schema::constraint_metadata::CatalogObjectAllocator;
        self.allocate_catalog_oid(class, &[0; 16])
            .map_err(|error| uqa_sql::catalog::errors::storage_error("graph OID", &error))
            .and_then(oid)
    }
}

fn oid(oid: i64) -> Result<u32, SQLError> {
    u32::try_from(oid).map_err(|_| SQLError::Internal(format!("invalid graph object OID {oid}")))
}
