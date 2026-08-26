//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `pg_class` and `pg_inherits` rows for physical and virtual relations.

use super::helpers::{
    bool_value, catalog_usize, int_value, row, split_schema_name, str_value, table_columns_for,
    view_columns_for,
};
use super::partitioning::partition_bound_node;
use super::pg_catalog::{
    catalog_index_relations, index_access_method_oid, pg_class_catalog_row, pg_class_row,
    pg_class_row_with_lifecycle, table_relation_oid,
};
use super::{Engine, ResultRow, SQLError};

pub(super) fn build_pg_class(engine: &Engine) -> Result<Vec<ResultRow>, SQLError> {
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
    let catalog_indexes = catalog_index_relations(engine)?;
    for name in engine
        .table_names()
        .map_err(|err| SQLError::Internal(format!("read table catalog: {err}")))?
    {
        let (schema, table) = split_schema_name(&name)?;
        let columns = table_columns_for(engine, &name)?;
        let hierarchy = engine
            .try_table_hierarchy(&name)
            .map_err(|error| SQLError::Internal(format!("read table hierarchy: {error}")))?;
        let relkind = if hierarchy.partition_spec.is_some() {
            "p"
        } else {
            "r"
        };
        let tuples = if hierarchy.partition_spec.is_some() {
            let mut total = 0_u64;
            for member in engine.hierarchy_scan_tables(&name, true)? {
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
            engine
                .table_persistence(&name)
                .map_err(|error| SQLError::Internal(format!("read table persistence: {error}")))?
                .unwrap_or_default(),
            true,
            &[],
        );
        row.insert(
            "relispartition".into(),
            bool_value(hierarchy.partition_bound.is_some()),
        );
        row.insert(
            "relhassubclass".into(),
            bool_value(!engine.direct_hierarchy_children(&name)?.is_empty()),
        );
        row.insert(
            "relhastriggers".into(),
            bool_value(engine.relation_has_triggers(&name)?),
        );
        row.insert(
            "relhasrules".into(),
            bool_value(engine.relation_has_rules(&name)?),
        );
        if let Some(bound) = hierarchy.partition_bound.as_ref() {
            row.insert(
                "relpartbound".into(),
                str_value(partition_bound_node(engine, &name, bound)?),
            );
        }
        out.push(row);
    }
    for name in engine.list_views()? {
        let (schema, view) = split_schema_name(&name)?;
        let columns = view_columns_for(engine, &name)?;
        let definition = engine.view_definition(&name)?.ok_or_else(|| {
            SQLError::Internal(format!("view `{name}` disappeared during catalog scan"))
        })?;
        out.push(pg_class_row_with_lifecycle(
            &schema,
            &view,
            "v",
            catalog_usize(columns.len(), "pg_class view column count")?,
            0.0,
            false,
            definition.persistence,
            true,
            &definition.options,
        ));
    }
    for name in engine.list_materialized_views()? {
        let (schema, view) = split_schema_name(&name)?;
        let definition = engine.view_definition(&name)?.ok_or_else(|| {
            SQLError::Internal(format!(
                "materialized view `{name}` disappeared during catalog scan"
            ))
        })?;
        let columns = view_columns_for(engine, &name)?;
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
    for name in engine.list_foreign_tables().map_err(SQLError::Internal)? {
        let (schema, table) = split_schema_name(&name)?;
        out.push(pg_class_row(
            &schema,
            &table,
            "f",
            catalog_usize(
                engine
                    .foreign_table_columns(&name)
                    .map_err(SQLError::Internal)?
                    .len(),
                "pg_class foreign-table column count",
            )?,
            0.0,
            false,
        ));
    }
    for sequence in engine
        .list_sequences()
        .map_err(|err| SQLError::Internal(format!("read sequence catalog: {err}")))?
    {
        let (schema, name) = split_schema_name(&sequence)?;
        out.push(pg_class_row_with_lifecycle(
            &schema,
            &name,
            "S",
            0,
            0.0,
            false,
            engine
                .sequence_persistence(&sequence)
                .map_err(|error| SQLError::Internal(format!("read sequence persistence: {error}")))?
                .unwrap_or_default(),
            true,
            &[],
        ));
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
    out.extend(super::ag_catalog::age_pg_class_rows(engine)?);
    Ok(out)
}

pub(super) fn build_pg_inherits(engine: &Engine) -> Result<Vec<ResultRow>, SQLError> {
    let mut out = Vec::new();
    for child in engine
        .table_names()
        .map_err(|error| SQLError::Internal(format!("read inheritance catalog: {error}")))?
    {
        let hierarchy = engine
            .try_table_hierarchy(&child)
            .map_err(|error| SQLError::Internal(format!("read inheritance metadata: {error}")))?;
        for (position, parent) in hierarchy.parents.iter().enumerate() {
            out.push(row([
                ("inhrelid", int_value(table_relation_oid(engine, &child)?)),
                ("inhparent", int_value(table_relation_oid(engine, parent)?)),
                (
                    "inhseqno",
                    int_value(i64::from(hierarchy.parent_sequence_number(position))),
                ),
                ("inhdetachpending", bool_value(false)),
            ]));
        }
    }
    for index in catalog_index_relations(engine)? {
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
