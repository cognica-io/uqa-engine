//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Index relation and catalog projection.

use uqa_core::Value;
use uqa_sql::{ResultRow, SQLError};

use crate::catalog::index::{index_definition, IndexDefinition};
use crate::catalog::{CatalogReadView, RelationNameResolution};
use uqa_core::RelationIdentity;

use super::super::helpers::index_definitions::{index_columns, indexdef};
use super::super::helpers::oids::{relation_oid, split_schema_name};
use super::super::helpers::rows::{
    bool_value, catalog_int2vector, catalog_ordinal, catalog_usize, int_value, row, str_value,
};
use super::table_relation_oid_from;

#[derive(Debug, Clone)]
pub struct CatalogIndexRelation {
    pub relation: RelationIdentity,
    pub table_name: String,
    pub index_type: String,
    pub columns: Vec<uqa_sql::ast::IndexKey>,
    pub definition: IndexDefinition,
    pub primary: bool,
    pub relkind: &'static str,
    pub is_partition: bool,
    pub has_children: bool,
    pub parent_index_oid: Option<i64>,
}

impl CatalogIndexRelation {
    pub fn oid(&self) -> i64 {
        self.definition.catalog.as_ref().map_or_else(
            || relation_oid(self.relkind, &self.relation.schema, &self.relation.name),
            |catalog| catalog.identity.oid,
        )
    }
}

pub(crate) mod legacy;

pub fn catalog_index_relations(
    catalog: &CatalogReadView,
    _resolution: &RelationNameResolution,
) -> Result<Vec<CatalogIndexRelation>, SQLError> {
    let mut rows = Vec::new();
    let mut addresses = std::collections::BTreeMap::new();
    let mut parents = std::collections::BTreeSet::new();
    for row in catalog.catalog_indexes() {
        let definition =
            index_definition(row).map_err(|error| SQLError::Internal(error.to_string()))?;
        let identity = definition.catalog.as_ref().ok_or_else(|| {
            SQLError::Internal(format!(
                "index `{}` has no catalog identity",
                row.relation.qualified_name()
            ))
        })?;
        addresses.insert(identity.identity.object_id, identity.identity.oid);
        parents.extend(definition.relationships.parent_index);
        rows.push((row, definition));
    }
    rows.into_iter()
        .map(|(row, definition)| {
            let table =
                RelationIdentity::from_legacy_name(&row.table_name).map_err(SQLError::Internal)?;
            let table = catalog
                .snapshot()
                .tables
                .get(&table)
                .ok_or_else(|| SQLError::UnknownTable(row.table_name.clone()))?;
            let identity = definition
                .catalog
                .as_ref()
                .expect("validated index identity");
            identity
                .validate(table.object_id)
                .map_err(|error| SQLError::Internal(error.to_string()))?;
            definition
                .relationships
                .validate(identity.identity.object_id)
                .map_err(|error| SQLError::Internal(error.to_string()))?;
            let primary = if let Some(owner) = definition.relationships.owning_constraint {
                let key = table
                    .keys
                    .iter()
                    .find(|key| key.catalog_identity.is_some_and(|id| id.object_id == owner))
                    .ok_or_else(|| {
                        SQLError::Internal(format!(
                            "index `{}` has no owning constraint",
                            row.relation.qualified_name()
                        ))
                    })?;
                key.kind == uqa_sql::ast::TableKeyConstraintKind::PrimaryKey
            } else {
                false
            };
            let parent_index_oid = definition
                .relationships
                .parent_index
                .map(|parent| {
                    addresses.get(&parent).copied().ok_or_else(|| {
                        SQLError::Internal(format!(
                            "index `{}` has no parent index",
                            row.relation.qualified_name()
                        ))
                    })
                })
                .transpose()?;
            Ok(CatalogIndexRelation {
                relation: row.relation.clone(),
                table_name: row.table_name.clone(),
                index_type: row.index_type.clone(),
                columns: index_columns(&row.columns_json)?,
                primary,
                relkind: if table.hierarchy.partition_spec.is_some() {
                    "I"
                } else {
                    "i"
                },
                is_partition: parent_index_oid.is_some(),
                has_children: parents.contains(&identity.identity.object_id),
                parent_index_oid,
                definition,
            })
        })
        .collect()
}

pub fn index_access_method_oid(method: &str) -> i64 {
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

fn index_key_ordinals(
    index: &CatalogIndexRelation,
    table_cols: &[uqa_sql::ast::ColumnDef],
) -> Result<Vec<i64>, SQLError> {
    index
        .columns
        .iter()
        .map(uqa_sql::ast::IndexKey::column)
        .chain(
            index
                .definition
                .included_columns
                .iter()
                .map(|name| Some(name.as_str())),
        )
        .map(|column| {
            column
                .and_then(|name| table_cols.iter().position(|item| item.name == name))
                .map(|position| catalog_ordinal(position, "pg_index key column"))
                .transpose()
                .map(|ordinal| ordinal.unwrap_or(0))
        })
        .collect()
}

pub fn build_pg_index(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
) -> Result<Vec<ResultRow>, SQLError> {
    let mut rows = Vec::new();
    for index in catalog_index_relations(catalog, resolution)? {
        let table_cols = &catalog
            .table(resolution, &index.table_name)?
            .ok_or_else(|| SQLError::UnknownTable(index.table_name.clone()))?
            .columns;
        let keys = index_key_ordinals(&index, table_cols)?;
        let expressions = index
            .columns
            .iter()
            .filter_map(|key| match key {
                uqa_sql::ast::IndexKey::Expression(expression) => Some(expression.as_ref()),
                uqa_sql::ast::IndexKey::Column(_) => None,
            })
            .map(|expression| {
                super::super::view_definition::stored_expression_definition(
                    catalog, resolution, expression, false,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        let column_count = catalog_usize(index.columns.len(), "pg_index column count")?;
        let total_count = catalog_usize(
            index.columns.len() + index.definition.included_columns.len(),
            "pg_index total column count",
        )?;
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
            ("indnatts", int_value(total_count)),
            ("indnkeyatts", int_value(column_count)),
            ("indisunique", bool_value(index.definition.unique)),
            (
                "indnullsnotdistinct",
                bool_value(index.definition.nulls_not_distinct),
            ),
            ("indisprimary", bool_value(index.primary)),
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
                catalog_int2vector(
                    keys.into_iter().map(Value::Int).collect(),
                    "pg_index.indkey",
                )?,
            ),
            ("indcollation", Value::Null),
            ("indclass", Value::Null),
            ("indoption", index_options(&index)?),
            (
                "indexprs",
                if expressions.is_empty() {
                    Value::Null
                } else {
                    Value::Str(expressions.join(", "))
                },
            ),
            (
                "indpred",
                index
                    .definition
                    .predicate
                    .as_deref()
                    .map(|predicate| {
                        super::super::view_definition::stored_expression_definition(
                            catalog, resolution, predicate, false,
                        )
                    })
                    .transpose()?
                    .map_or(Value::Null, Value::Str),
            ),
        ]));
    }
    Ok(rows)
}

fn index_options(index: &CatalogIndexRelation) -> Result<Value, SQLError> {
    catalog_int2vector(
        (0..index.columns.len())
            .map(|position| {
                let order = index
                    .definition
                    .column_order
                    .get(position)
                    .copied()
                    .unwrap_or_default();
                Value::Int(i64::from(order.descending) + 2 * i64::from(order.nulls_first))
            })
            .collect(),
        "pg_index.indoption",
    )
}

pub fn build_pg_indexes(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
) -> Result<Vec<ResultRow>, SQLError> {
    let mut rows = Vec::new();
    for index in catalog_index_relations(catalog, resolution)? {
        let (schema, table) = split_schema_name(&index.table_name)?;
        let qualified_table = format!(
            "{}.{}",
            uqa_sql::expr::quote_ident(if schema.starts_with("pg_temp_") {
                "pg_temp"
            } else {
                &schema
            }),
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
            ("indexname", str_value(index.relation.name.clone())),
            ("tablespace", Value::Null),
            (
                "indexdef",
                str_value(indexdef(catalog, resolution, &index, &index_target, false)?),
            ),
        ]));
    }
    Ok(rows)
}
