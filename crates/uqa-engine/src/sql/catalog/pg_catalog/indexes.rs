//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Index relation and catalog projection.

use uqa_core::Value;
use uqa_sql::{ResultRow, SQLError};

use crate::engine_capabilities::{CatalogReadView, RelationNameResolution};

use super::super::helpers::index_definitions::{index_columns, indexdef};
use super::super::helpers::oids::{relation_oid, split_index_name, split_schema_name};
use super::super::helpers::rows::{
    bool_value, catalog_ordinal, catalog_usize, int_value, row, str_value,
};
use super::table_relation_oid_from;

#[derive(Debug, Clone)]
pub(in crate::sql::catalog) struct CatalogIndexRelation {
    pub(in crate::sql::catalog) schema: String,
    pub(in crate::sql::catalog) name: String,
    pub(in crate::sql::catalog) table_name: String,
    pub(in crate::sql::catalog) index_type: String,
    pub(in crate::sql::catalog) columns: Vec<String>,
    pub(in crate::sql::catalog) relkind: &'static str,
    pub(in crate::sql::catalog) is_partition: bool,
    pub(in crate::sql::catalog) has_children: bool,
    pub(in crate::sql::catalog) parent_index_oid: Option<i64>,
}

impl CatalogIndexRelation {
    pub(in crate::sql::catalog) fn oid(&self) -> i64 {
        relation_oid(self.relkind, &self.schema, &self.name)
    }
}

pub(in crate::sql::catalog) fn catalog_index_relations(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
) -> Result<Vec<CatalogIndexRelation>, SQLError> {
    let registered = catalog.catalog_indexes().cloned().collect::<Vec<_>>();
    let mut used = registered
        .iter()
        .map(|index| index.name.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let mut output = Vec::new();
    for index in registered {
        let (table_schema, _) = split_schema_name(&index.table_name)?;
        let (schema, name) = split_index_name(&index.name, &table_schema)?;
        let columns = index_columns(&index.columns_json)?;
        let hierarchy = &catalog
            .table(resolution, &index.table_name)?
            .ok_or_else(|| SQLError::UnknownTable(index.table_name.clone()))?
            .hierarchy;
        let relkind = if hierarchy.partition_spec.is_some() {
            "I"
        } else {
            "i"
        };
        let has_children = relkind == "I"
            && !catalog
                .direct_hierarchy_children(resolution, &index.table_name)?
                .is_empty();
        let root = CatalogIndexRelation {
            schema,
            name,
            table_name: index.table_name.clone(),
            index_type: index.index_type.clone(),
            columns: columns.clone(),
            relkind,
            is_partition: false,
            has_children,
            parent_index_oid: None,
        };
        let root_oid = root.oid();
        output.push(root);
        if relkind == "I" {
            append_partition_index_children(
                catalog,
                resolution,
                &index.table_name,
                root_oid,
                &index.index_type,
                &columns,
                &mut used,
                &mut output,
            )?;
        }
    }
    Ok(output)
}

#[allow(clippy::too_many_arguments)]
fn append_partition_index_children(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    parent_table: &str,
    parent_index_oid: i64,
    index_type: &str,
    columns: &[String],
    used: &mut std::collections::BTreeSet<String>,
    output: &mut Vec<CatalogIndexRelation>,
) -> Result<(), SQLError> {
    for child in catalog.direct_hierarchy_children(resolution, parent_table)? {
        let (schema, table) = split_schema_name(&child)?;
        let hierarchy = &catalog
            .table(resolution, &child)?
            .ok_or_else(|| SQLError::UnknownTable(child.clone()))?
            .hierarchy;
        let relkind = if hierarchy.partition_spec.is_some() {
            "I"
        } else {
            "i"
        };
        let children = catalog.direct_hierarchy_children(resolution, &child)?;
        let name = allocate_derived_index_name(&table, columns, used);
        let relation = CatalogIndexRelation {
            schema,
            name,
            table_name: child.clone(),
            index_type: index_type.to_string(),
            columns: columns.to_vec(),
            relkind,
            is_partition: true,
            has_children: !children.is_empty(),
            parent_index_oid: Some(parent_index_oid),
        };
        let relation_oid = relation.oid();
        output.push(relation);
        if relkind == "I" {
            append_partition_index_children(
                catalog,
                resolution,
                &child,
                relation_oid,
                index_type,
                columns,
                used,
                output,
            )?;
        }
    }
    Ok(())
}

fn allocate_derived_index_name(
    table: &str,
    columns: &[String],
    used: &mut std::collections::BTreeSet<String>,
) -> String {
    fn component(raw: &str) -> String {
        let mut output = String::with_capacity(raw.len());
        let mut separator = false;
        for character in raw.chars() {
            if character.is_alphanumeric() || character == '_' {
                output.extend(character.to_lowercase());
                separator = false;
            } else if !separator && !output.is_empty() {
                output.push('_');
                separator = true;
            }
        }
        while output.ends_with('_') {
            output.pop();
        }
        output
    }

    let mut parts = std::iter::once(table)
        .chain(columns.iter().map(String::as_str))
        .map(component)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if parts.is_empty() {
        parts.push("index".into());
    }
    let base = format!("{}_idx", parts.join("_"));
    if used.insert(base.clone()) {
        return base;
    }
    for suffix in 1_u64.. {
        let candidate = format!("{base}_{suffix}");
        if used.insert(candidate.clone()) {
            return candidate;
        }
    }
    unreachable!("u64 index-name suffix space is non-empty")
}

pub(in crate::sql::catalog) fn index_access_method_oid(method: &str) -> i64 {
    match method.to_ascii_lowercase().as_str() {
        "" | "btree" => 403,
        "hash" => 405,
        "gist" => 783,
        "gin" => 2_742,
        "spgist" => 4_000,
        "brin" => 3_580,
        _ => 0,
    }
}

pub(in crate::sql::catalog) fn build_pg_index(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
) -> Result<Vec<ResultRow>, SQLError> {
    let mut rows = Vec::new();
    for index in catalog_index_relations(catalog, resolution)? {
        let table_cols = &catalog
            .table(resolution, &index.table_name)?
            .ok_or_else(|| SQLError::UnknownTable(index.table_name.clone()))?
            .columns;
        let mut keys = Vec::with_capacity(index.columns.len());
        for column in &index.columns {
            if let Some(position) = table_cols.iter().position(|item| item.name == *column) {
                keys.push(catalog_ordinal(position, "pg_index key column")?);
            }
        }
        let column_count = catalog_usize(index.columns.len(), "pg_index column count")?;
        rows.push(row([
            ("indexrelid", int_value(index.oid())),
            (
                "indrelid",
                int_value(table_relation_oid_from(
                    catalog,
                    resolution,
                    &index.table_name,
                )?),
            ),
            ("indnatts", int_value(column_count)),
            ("indnkeyatts", int_value(column_count)),
            ("indisunique", bool_value(false)),
            ("indnullsnotdistinct", bool_value(false)),
            ("indisprimary", bool_value(false)),
            ("indisexclusion", bool_value(false)),
            ("indimmediate", bool_value(true)),
            ("indisclustered", bool_value(false)),
            ("indisvalid", bool_value(true)),
            ("indcheckxmin", bool_value(false)),
            ("indisready", bool_value(true)),
            ("indislive", bool_value(true)),
            ("indisreplident", bool_value(false)),
            (
                "indkey",
                Value::List(keys.into_iter().map(Value::Int).collect()),
            ),
            ("indcollation", Value::Null),
            ("indclass", Value::Null),
            ("indoption", Value::Null),
            ("indexprs", Value::Null),
            ("indpred", Value::Null),
        ]));
    }
    Ok(rows)
}

pub(in crate::sql::catalog) fn build_pg_indexes(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
) -> Result<Vec<ResultRow>, SQLError> {
    let mut rows = Vec::new();
    for index in catalog_index_relations(catalog, resolution)? {
        let (schema, table) = split_schema_name(&index.table_name)?;
        let qualified_table = format!(
            "{}.{}",
            uqa_sql::expr::quote_ident(&schema),
            uqa_sql::expr::quote_ident(&table)
        );
        let index_target = if index.relkind == "I" {
            format!("ONLY {qualified_table}")
        } else {
            qualified_table
        };
        rows.push(row([
            ("schemaname", str_value(schema)),
            ("tablename", str_value(table.clone())),
            ("indexname", str_value(index.name.clone())),
            ("tablespace", Value::Null),
            (
                "indexdef",
                str_value(indexdef(
                    &index.name,
                    &index.index_type,
                    &index_target,
                    &index.columns,
                )),
            ),
        ]));
    }
    Ok(rows)
}
