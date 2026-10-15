//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Join visible physical constraint rows to relation and attribute identities without deduplicating OIDs.

use super::super::{
    helpers::constraints::{
        constraint_catalog_rows, foreign_constraint_catalog_rows, referenced_key_positions,
        table_constraint_catalog_rows, ConstraintCatalogColumn, ConstraintCatalogKind,
        ConstraintCatalogRow, ConstraintCatalogState, ConstraintDeferralState,
        ConstraintInheritanceState, ConstraintValidationState, ForeignKeyCatalogData,
    },
    helpers::oids::namespace_oid,
    CatalogRequest,
};
use crate::catalog::{
    tuples::constraint::ConstraintTuple, CatalogReadView, RelationNameResolution,
};
use uqa_core::{RelationIdentity, Value};
use uqa_sql::{
    ast::{ColumnDef, ForeignKeyAction, ForeignKeyMatch, RelationPersistence},
    catalog::node_tree::Node,
    ResultRow, SQLError,
};

pub(super) struct RelationConstraint {
    pub definition: ConstraintCatalogRow,
    pub node: Option<Node>,
    generation: u64,
}

pub(super) struct VisibleTuple {
    tuple: ConstraintTuple,
    generation: u64,
}
impl std::ops::Deref for VisibleTuple {
    type Target = ConstraintTuple;
    fn deref(&self) -> &Self::Target {
        &self.tuple
    }
}

impl std::ops::Deref for RelationConstraint {
    type Target = ConstraintCatalogRow;
    fn deref(&self) -> &Self::Target {
        &self.definition
    }
}

pub(super) fn stored(
    catalog: &CatalogReadView,
    request: &CatalogRequest,
    expression: bool,
) -> Result<Option<Vec<VisibleTuple>>, SQLError> {
    let Some(source) = &catalog.physical_catalog else {
        return Ok(None);
    };
    let mut rows = Vec::new();
    crate::catalog::tuples::visit(
        source.as_ref(),
        uqa_sql::schema::constraint_metadata::CatalogOidClass::Constraint,
        &mut |physical, payload| {
            if let Some(tuple) =
                ConstraintTuple::read_selected(payload.get(), expression, |fields| {
                    fields.get("conname").is_none_or(|value| match value {
                        Value::Str(name) => request.matches_name("constraint_name", name),
                        _ => true,
                    })
                })?
            {
                rows.push(VisibleTuple {
                    tuple,
                    generation: physical.generation(),
                });
            }
            Ok(())
        },
    )
    .map_err(|error| {
        uqa_sql::catalog::errors::storage_error("read constraint view rows", &error)
    })?;
    Ok(Some(rows))
}

pub(super) fn relations(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    request: &CatalogRequest,
) -> Result<Vec<RelationConstraint>, SQLError> {
    let Some(stored) = stored(catalog, request, request.includes("check_clause"))? else {
        return Ok(constraint_catalog_rows(catalog, resolution)?
            .iter()
            .cloned()
            .map(|definition| RelationConstraint {
                definition,
                node: None,
                generation: 0,
            })
            .collect());
    };
    let mut rows = Vec::new();
    for tuple in stored {
        if let Some(definition) = relation(catalog, tuple.fields())? {
            rows.push(RelationConstraint {
                definition,
                node: tuple.expression().cloned(),
                generation: tuple.generation,
            });
        }
    }
    for (relation, table) in &catalog.snapshot().tables {
        if table.persistence == RelationPersistence::Temporary {
            rows.extend(
                table_constraint_catalog_rows(
                    catalog,
                    resolution,
                    &relation.qualified_name(),
                    table,
                )?
                .into_iter()
                .map(|definition| RelationConstraint {
                    definition,
                    node: None,
                    generation: 0,
                }),
            );
        }
    }
    for (relation, table) in &*catalog.snapshot().definitions.foreign_tables {
        if table.persistence == RelationPersistence::Temporary {
            rows.extend(
                foreign_constraint_catalog_rows(&relation.qualified_name(), table.clone())?
                    .into_iter()
                    .map(|definition| RelationConstraint {
                        definition,
                        node: None,
                        generation: 0,
                    }),
            );
        }
    }
    Ok(rows)
}

fn relation(
    catalog: &CatalogReadView,
    fields: &ResultRow,
) -> Result<Option<ConstraintCatalogRow>, SQLError> {
    let oid = integer(fields, "conrelid")?;
    let kind = string(fields, "contype")?;
    if oid == 0 || kind == "t" {
        return Ok(None);
    }
    let Some((relation, columns)) = relation_columns(catalog, oid) else {
        return Ok(None);
    };
    let index =
        super::super::pg_catalog::catalog_index_by_oid(catalog, integer(fields, "conindid")?)?;
    let kind = match kind {
        "n" => ConstraintCatalogKind::NotNull,
        "c" => ConstraintCatalogKind::Check,
        "p" => ConstraintCatalogKind::PrimaryKey,
        "u" => ConstraintCatalogKind::Unique {
            nulls_not_distinct: index.is_some_and(|index| index.definition.nulls_not_distinct),
        },
        "f" => ConstraintCatalogKind::ForeignKey,
        _ => return Err(corrupt("invalid constraint kind")),
    };
    let own_columns = numbers(fields, "conkey")?
        .into_iter()
        .filter_map(|number| {
            attribute_name(columns, number).map(|name| ConstraintCatalogColumn {
                name: name.into(),
                table_ordinal: number,
            })
        })
        .collect();
    let foreign_key = if kind == ConstraintCatalogKind::ForeignKey {
        let Some((referenced, columns)) = relation_columns(catalog, integer(fields, "confrelid")?)
        else {
            return Ok(None);
        };
        let ordinals = numbers(fields, "confkey")?;
        let names = ordinals
            .iter()
            .filter_map(|&number| attribute_name(columns, number).map(str::to_owned))
            .collect::<Vec<_>>();
        let positions =
            referenced_key_positions(&names, index.map(|index| index.columns.as_slice()))?;
        Some(ForeignKeyCatalogData {
            enforcement_identity: None,
            referenced_index: index.and_then(|index| {
                index
                    .definition
                    .catalog
                    .as_ref()
                    .map(|identity| identity.identity.object_id)
            }),
            schema: referenced.schema.clone(),
            table: referenced.name.clone(),
            column_ordinals: ordinals,
            delete_set_ordinals: numbers(fields, "confdelsetcols")?,
            positions_in_unique_constraint: positions,
            on_update: action(string(fields, "confupdtype")?)?,
            on_delete: action(string(fields, "confdeltype")?)?,
            match_type: match string(fields, "confmatchtype")? {
                "s" => ForeignKeyMatch::Simple,
                "f" => ForeignKeyMatch::Full,
                _ => return Err(corrupt("invalid foreign-key match type")),
            },
        })
    } else {
        None
    };
    let Some(schema) = namespace(catalog, integer(fields, "connamespace")?) else {
        return Ok(None);
    };
    Ok(Some(ConstraintCatalogRow {
        schema,
        table: relation.name.clone(),
        name: string(fields, "conname")?.into(),
        object_id: None,
        catalog_oid: Some(integer(fields, "oid")?),
        kind,
        columns: own_columns,
        state: ConstraintCatalogState::new(
            ConstraintValidationState::new(
                boolean(fields, "conenforced")?,
                boolean(fields, "convalidated")?,
            ),
            ConstraintDeferralState::new(
                boolean(fields, "condeferrable")?,
                boolean(fields, "condeferred")?,
            ),
            ConstraintInheritanceState::new(boolean(fields, "connoinherit")?),
        ),
        period: boolean(fields, "conperiod")?,
        foreign_key,
        expression: None,
        parent_oid: match integer(fields, "conparentid")? {
            0 => None,
            oid => Some(oid),
        },
    }))
}

pub(super) fn namespace(catalog: &CatalogReadView, oid: i64) -> Option<String> {
    catalog
        .all_schema_names()
        .into_iter()
        .find(|schema| namespace_oid(catalog, schema) == oid)
}

fn relation_columns(
    catalog: &CatalogReadView,
    oid: i64,
) -> Option<(&RelationIdentity, &[ColumnDef])> {
    catalog
        .snapshot()
        .tables
        .iter()
        .find(|(_, table)| i64::from(table.catalog_oids.relation) == oid)
        .map(|(relation, table)| (relation, table.columns.as_slice()))
        .or_else(|| {
            catalog
                .snapshot()
                .definitions
                .foreign_tables
                .iter()
                .find(|(_, table)| i64::from(table.relation_oids().relation) == oid)
                .map(|(relation, table)| (relation, table.columns.as_slice()))
        })
}

fn attribute_name(columns: &[ColumnDef], number: i64) -> Option<&str> {
    if number == -6 {
        return Some("tableoid");
    }
    uqa_sql::catalog::relation_attributes::column_by_number(columns, number)
        .map(|column| column.name.as_str())
}

pub(super) fn integer(fields: &ResultRow, name: &str) -> Result<i64, SQLError> {
    match fields.get(name) {
        Some(Value::Int(value)) => Ok(*value),
        _ => Err(corrupt(name)),
    }
}
pub(super) fn string<'a>(fields: &'a ResultRow, name: &str) -> Result<&'a str, SQLError> {
    match fields.get(name) {
        Some(Value::Str(value)) => Ok(value),
        _ => Err(corrupt(name)),
    }
}
fn boolean(fields: &ResultRow, name: &str) -> Result<bool, SQLError> {
    match fields.get(name) {
        Some(Value::Bool(value)) => Ok(*value),
        _ => Err(corrupt(name)),
    }
}
fn numbers(fields: &ResultRow, name: &str) -> Result<Vec<i64>, SQLError> {
    match fields.get(name) {
        Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(array)) => array
            .elements()
            .iter()
            .map(|value| match value {
                Value::Int(n) => Ok(*n),
                _ => Err(corrupt(name)),
            })
            .collect(),
        _ => Err(corrupt(name)),
    }
}
fn action(value: &str) -> Result<ForeignKeyAction, SQLError> {
    match value {
        "a" => Ok(ForeignKeyAction::NoAction),
        "r" => Ok(ForeignKeyAction::Restrict),
        "c" => Ok(ForeignKeyAction::Cascade),
        "n" => Ok(ForeignKeyAction::SetNull),
        "d" => Ok(ForeignKeyAction::SetDefault),
        _ => Err(corrupt("invalid referential action")),
    }
}
fn corrupt(field: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "XX001".into(),
        message: format!("invalid physical constraint field: {field}"),
    }
}

/// The CHECK arm of `constraint_column_usage` joins `pg_depend` to a unique `pg_constraint` OID lookup. `PostgreSQL`'s nested-loop lookup stops at the first visible physical version; the direct scan and the other view arms retain their full bag.
pub(super) fn dependency_lookup(rows: Vec<RelationConstraint>) -> Vec<RelationConstraint> {
    let mut selected = std::collections::BTreeMap::new();
    for row in &rows {
        if row.kind == ConstraintCatalogKind::Check {
            selected
                .entry(row.catalog_oid)
                .and_modify(|generation: &mut u64| *generation = (*generation).min(row.generation))
                .or_insert(row.generation);
        }
    }
    rows.into_iter()
        .filter(|row| {
            row.kind != ConstraintCatalogKind::Check
                || selected.get(&row.catalog_oid) == Some(&row.generation)
        })
        .collect()
}
