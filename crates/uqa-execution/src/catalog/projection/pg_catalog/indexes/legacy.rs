//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Preserve predecessor index names and addresses only during initial catalog conversion.

use super::{
    index_columns, index_definition, split_schema_name, CatalogIndexRelation, CatalogReadView,
    IndexDefinition, RelationIdentity, RelationNameResolution, SQLError,
};

pub(crate) fn catalog_index_relations(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
) -> Result<Vec<CatalogIndexRelation>, SQLError> {
    let mut roots = Vec::new();
    for index in catalog.catalog_indexes() {
        roots.push(CatalogIndexRelation {
            relation: index.relation.clone(),
            table_name: index.table_name.clone(),
            index_type: index.index_type.clone(),
            columns: index_columns(&index.columns_json)?,
            definition: index_definition(index)
                .map_err(|error| SQLError::Internal(error.to_string()))?,
            primary: false,
            relkind: "i",
            is_partition: false,
            has_children: false,
            parent_index_oid: None,
        });
    }
    for table in catalog.table_names() {
        let snapshot = catalog
            .table(resolution, &table)?
            .ok_or_else(|| SQLError::UnknownTable(table.clone()))?;
        let (schema, _) = split_schema_name(&table)?;
        for key in snapshot.keys.iter() {
            let name = key
                .name
                .as_ref()
                .ok_or_else(|| SQLError::Internal("unnamed catalog key".into()))?;
            roots.push(CatalogIndexRelation {
                relation: RelationIdentity::new(&schema, name),
                table_name: table.clone(),
                index_type: if key.without_overlaps {
                    "gist"
                } else {
                    "btree"
                }
                .into(),
                columns: key
                    .columns
                    .iter()
                    .cloned()
                    .map(uqa_sql::ast::IndexKey::Column)
                    .collect(),
                definition: IndexDefinition {
                    unique: true,
                    nulls_not_distinct: key.nulls_not_distinct,
                    ..IndexDefinition::default()
                },
                primary: key.kind == uqa_sql::ast::TableKeyConstraintKind::PrimaryKey,
                relkind: "i",
                is_partition: false,
                has_children: false,
                parent_index_oid: None,
            });
        }
    }
    let mut used = roots
        .iter()
        .map(|index| index.relation.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let mut order = roots
        .iter()
        .map(|index| {
            partition_depth(catalog, resolution, &index.table_name)
                .map(|depth| (depth, index.relation.clone()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    order.sort();
    let mut pending = roots
        .into_iter()
        .map(|index| (index.relation.clone(), index))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut output = Vec::new();
    for (_, relation) in order {
        if let Some(root) = pending.remove(&relation) {
            append_index_tree(
                catalog,
                resolution,
                root,
                &mut used,
                &mut pending,
                &mut output,
            )?;
        }
    }
    Ok(output)
}

fn append_index_tree(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    mut index: CatalogIndexRelation,
    used: &mut std::collections::BTreeSet<RelationIdentity>,
    pending: &mut std::collections::BTreeMap<RelationIdentity, CatalogIndexRelation>,
    output: &mut Vec<CatalogIndexRelation>,
) -> Result<(), SQLError> {
    let snapshot = catalog
        .table(resolution, &index.table_name)?
        .ok_or_else(|| SQLError::UnknownTable(index.table_name.clone()))?;
    index.relkind = if snapshot.hierarchy.partition_spec.is_some() {
        "I"
    } else {
        "i"
    };
    let children = if index.relkind == "I" {
        catalog.direct_hierarchy_children(resolution, &index.table_name)?
    } else {
        Vec::new()
    };
    index.has_children = !children.is_empty();
    let parent_oid = index.oid();
    output.push(index.clone());
    for child in children {
        let (schema, table) = split_schema_name(&child)?;
        let reusable = pending
            .values()
            .find(|candidate| candidate.table_name == child && equivalent_index(candidate, &index))
            .map(|candidate| candidate.relation.clone());
        let mut child_index = if let Some(reusable) = reusable {
            pending
                .remove(&reusable)
                .ok_or_else(|| SQLError::Internal("partition index disappeared".into()))?
        } else {
            let mut definition = index.definition.clone();
            definition.catalog = None;
            CatalogIndexRelation {
                definition,
                relation: allocate_derived_index_name(
                    &schema,
                    &table,
                    &index
                        .columns
                        .iter()
                        .map(|key| key.column().unwrap_or("expr").to_owned())
                        .collect::<Vec<_>>(),
                    used,
                ),
                table_name: child,
                ..index.clone()
            }
        };
        child_index.is_partition = true;
        child_index.parent_index_oid = Some(parent_oid);
        append_index_tree(catalog, resolution, child_index, used, pending, output)?;
    }
    Ok(())
}

fn equivalent_index(left: &CatalogIndexRelation, right: &CatalogIndexRelation) -> bool {
    left.columns == right.columns
        && left.primary == right.primary
        && left.index_type == right.index_type
        && left.definition.unique == right.definition.unique
        && left.definition.nulls_not_distinct == right.definition.nulls_not_distinct
        && left.definition.included_columns == right.definition.included_columns
        && left.definition.predicate == right.definition.predicate
        && (0..left.columns.len()).all(|position| {
            left.definition
                .column_order
                .get(position)
                .copied()
                .unwrap_or_default()
                == right
                    .definition
                    .column_order
                    .get(position)
                    .copied()
                    .unwrap_or_default()
        })
}

fn partition_depth(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    table: &str,
) -> Result<usize, SQLError> {
    let mut table = table.to_string();
    let mut ancestors = std::collections::BTreeSet::new();
    loop {
        let snapshot = catalog
            .table(resolution, &table)?
            .ok_or_else(|| SQLError::UnknownTable(table.clone()))?;
        if snapshot.hierarchy.partition_bound.is_none() {
            return Ok(ancestors.len());
        }
        let Some(parent) = snapshot.hierarchy.parents.first() else {
            return Ok(ancestors.len());
        };
        if !ancestors.insert(parent.clone()) {
            return Err(SQLError::Internal("cyclic index partition ancestry".into()));
        }
        table.clone_from(parent);
    }
}

fn allocate_derived_index_name(
    schema: &str,
    table: &str,
    columns: &[String],
    used: &mut std::collections::BTreeSet<RelationIdentity>,
) -> RelationIdentity {
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
    let base_relation = RelationIdentity::new(schema, &base);
    if used.insert(base_relation.clone()) {
        return base_relation;
    }
    for suffix in 1_u64.. {
        let candidate = format!("{base}{suffix}");
        let relation = RelationIdentity::new(schema, candidate);
        if used.insert(relation.clone()) {
            return relation;
        }
    }
    unreachable!("u64 index-name suffix space is non-empty")
}
