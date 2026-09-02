//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `pg_class` and `pg_inherits` rows for physical and virtual relations.

use super::helpers::acl::acl_identifier;
use super::helpers::oids::{split_schema_name, stable_object_oid};
use super::helpers::rows::{bool_value, catalog_array, catalog_usize, int_value, row, str_value};
use super::helpers::views::view_columns_for;
use super::partitioning::partition_bound_node;
use super::pg_catalog::{
    catalog_index_relations, index_access_method_oid, pg_class_catalog_row, pg_class_row,
    pg_class_row_with_lifecycle, table_relation_oid_from, table_rowtype_oid_from,
};
use super::{Engine, ResultRow, SQLError};
use crate::engine_capabilities::{CatalogReadView, RelationNameResolution};

#[expect(
    clippy::too_many_lines,
    reason = "preserves catalog column and OID order"
)]
pub(super) fn build_pg_class(
    engine: &Engine,
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
) -> Result<Vec<ResultRow>, SQLError> {
    let mut out = vec![pg_class_catalog_row(
        13_313,
        13_315,
        "information_schema",
        "information_schema_catalog_name",
        "v",
        1,
        -1.0,
        false,
    )];
    let catalog_indexes = catalog_index_relations(catalog, resolution)?;
    for name in catalog.table_names() {
        let (schema, table) = split_schema_name(&name)?;
        let table_snapshot = catalog
            .table(resolution, &name)?
            .ok_or_else(|| SQLError::UnknownTable(name.clone()))?;
        let columns = &table_snapshot.columns;
        let hierarchy = &table_snapshot.hierarchy;
        let relkind = if hierarchy.partition_spec.is_some() {
            "p"
        } else {
            "r"
        };
        let tuples = if hierarchy.partition_spec.is_some() {
            let mut total = 0_u64;
            for member in catalog.hierarchy_scan_tables(resolution, &name, true)? {
                total = total
                    .checked_add(engine.table_doc_count(&member)?)
                    .ok_or_else(|| {
                        SQLError::Internal(format!(
                            "pg_class.reltuples overflow for hierarchy `{name}`"
                        ))
                    })?;
            }
            total
        } else {
            engine.table_doc_count(&name)?
        };
        let mut row = pg_class_row_with_lifecycle(
            &schema,
            &table,
            relkind,
            catalog_usize(columns.len(), "pg_class column count")?,
            tuples as f64,
            catalog_indexes.iter().any(|index| index.table_name == name),
            table_snapshot.persistence,
            true,
            &[],
        );
        row.insert(
            "oid".into(),
            int_value(table_relation_oid_from(catalog, resolution, &name)?),
        );
        row.insert(
            "reltype".into(),
            int_value(table_rowtype_oid_from(catalog, resolution, &name)?),
        );
        row.insert(
            "relispartition".into(),
            bool_value(hierarchy.partition_bound.is_some()),
        );
        row.insert(
            "relhassubclass".into(),
            bool_value(
                !catalog
                    .direct_hierarchy_children(resolution, &name)?
                    .is_empty(),
            ),
        );
        row.insert(
            "relhastriggers".into(),
            bool_value(catalog.relation_has_triggers(resolution, &name)?),
        );
        row.insert(
            "relhasrules".into(),
            bool_value(catalog.table_has_rules(resolution, &name)?),
        );
        if let Some(bound) = hierarchy.partition_bound.as_ref() {
            row.insert(
                "relpartbound".into(),
                str_value(partition_bound_node(
                    engine, catalog, resolution, &name, bound,
                )?),
            );
        }
        out.push(row);
    }
    for (name, definition) in catalog.views_of_kind(crate::StoredViewKind::View) {
        let (schema, view) = split_schema_name(&name)?;
        let columns = view_columns_for(engine, catalog, resolution, &definition)?;
        let mut row = pg_class_row_with_lifecycle(
            &schema,
            &view,
            "v",
            catalog_usize(columns.len(), "pg_class view column count")?,
            0.0,
            false,
            definition.persistence,
            true,
            &definition.options,
        );
        row.insert(
            "relhastriggers".into(),
            bool_value(catalog.relation_has_triggers(resolution, &name)?),
        );
        out.push(row);
    }
    for (name, definition) in catalog.views_of_kind(crate::StoredViewKind::Materialized) {
        let (schema, view) = split_schema_name(&name)?;
        let columns = view_columns_for(engine, catalog, resolution, &definition)?;
        out.push(pg_class_row_with_lifecycle(
            &schema,
            &view,
            "m",
            catalog_usize(columns.len(), "pg_class materialized-view column count")?,
            definition.materialized_rows.len() as f64,
            false,
            definition.persistence,
            definition.populated,
            &definition.options,
        ));
    }
    for (name, foreign_table) in catalog.foreign_tables() {
        let (schema, table) = split_schema_name(&name)?;
        out.push(pg_class_row(
            &schema,
            &table,
            "f",
            catalog_usize(
                foreign_table.columns.len(),
                "pg_class foreign-table column count",
            )?,
            0.0,
            false,
        ));
    }
    for (sequence, persistence, object_id, security) in catalog.sequences() {
        let (schema, name) = split_schema_name(&sequence)?;
        let mut row =
            pg_class_row_with_lifecycle(&schema, &name, "S", 3, 0.0, false, persistence, true, &[]);
        row.insert(
            "oid".into(),
            int_value(stable_object_oid("relation", &object_id)),
        );
        row.insert(
            "relowner".into(),
            int_value(crate::engine_roles::role_oid(&security.role_owner)),
        );
        row.insert("relacl".into(), sequence_acl_catalog_value(&security)?);
        out.push(row);
    }
    for index in catalog_indexes {
        let mut index_row = pg_class_row(
            &index.schema,
            &index.name,
            index.relkind,
            catalog_usize(index.columns.len(), "pg_class index column count")?,
            0.0,
            false,
        );
        index_row.insert(
            "relam".into(),
            int_value(index_access_method_oid(&index.index_type)),
        );
        index_row.insert("relispartition".into(), bool_value(index.is_partition));
        index_row.insert("relhassubclass".into(), bool_value(index.has_children));
        out.push(index_row);
    }
    out.extend(super::ag_catalog::age_pg_class_rows(catalog)?);
    Ok(out)
}

fn sequence_acl_catalog_value(
    security: &crate::engine_state::SequenceSecurity,
) -> Result<uqa_core::Value, SQLError> {
    let Some(acl) = security.acl.as_ref() else {
        return Ok(uqa_core::Value::Null);
    };
    catalog_array(
        acl.iter()
            .map(|entry| {
                let grantee = if entry.role == "PUBLIC" {
                    String::new()
                } else {
                    acl_identifier(&entry.role)
                };
                let grantor =
                    acl_identifier(entry.grantor.as_deref().unwrap_or(&security.role_owner));
                let mut privileges = String::new();
                for (enabled, grant_option, code) in [
                    (entry.privileges.select, entry.grant_options.select, 'r'),
                    (entry.privileges.update, entry.grant_options.update, 'w'),
                    (entry.privileges.usage, entry.grant_options.usage, 'U'),
                ] {
                    if enabled {
                        privileges.push(code);
                        if grant_option {
                            privileges.push('*');
                        }
                    }
                }
                str_value(format!("{grantee}={privileges}/{grantor}"))
            })
            .collect(),
        "pg_class.relacl",
    )
}

pub(super) fn build_pg_inherits(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
) -> Result<Vec<ResultRow>, SQLError> {
    let mut out = Vec::new();
    for child in catalog.table_names() {
        let hierarchy = &catalog
            .table(resolution, &child)?
            .ok_or_else(|| SQLError::UnknownTable(child.clone()))?
            .hierarchy;
        for (position, parent) in hierarchy.parents.iter().enumerate() {
            out.push(row([
                (
                    "inhrelid",
                    int_value(table_relation_oid_from(catalog, resolution, &child)?),
                ),
                (
                    "inhparent",
                    int_value(table_relation_oid_from(catalog, resolution, parent)?),
                ),
                (
                    "inhseqno",
                    int_value(i64::from(hierarchy.parent_sequence_number(position))),
                ),
                ("inhdetachpending", bool_value(false)),
            ]));
        }
    }
    for index in catalog_index_relations(catalog, resolution)? {
        if let Some(parent_oid) = index.parent_index_oid {
            out.push(row([
                ("inhrelid", int_value(index.oid())),
                ("inhparent", int_value(parent_oid)),
                ("inhseqno", int_value(1)),
                ("inhdetachpending", bool_value(false)),
            ]));
        }
    }
    Ok(out)
}
