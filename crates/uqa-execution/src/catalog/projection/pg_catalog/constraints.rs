//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Constraint catalog projection.

use uqa_core::Value;
use uqa_sql::{ResultRow, SQLError};

use crate::catalog::tuples::constraint::ConstraintTuple;
use crate::catalog::{CatalogReadView, RelationNameResolution};

use super::super::helpers::constraints::{
    constraint_catalog_rows, ConstraintCatalogKind, ConstraintCatalogRow,
};
use super::super::helpers::oids::{namespace_oid, stable_object_oid, stable_oid};
use super::super::helpers::rows::{
    bool_value, catalog_array, catalog_usize, int_value, row, str_value,
};
use super::table_relation_oid_from;

pub(crate) fn build_requested_pg_constraint(
    context: &crate::catalog::context::CatalogContext<'_>,
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    request: &super::super::CatalogRequest,
) -> Result<Vec<ResultRow>, SQLError> {
    if let Some(source) = &catalog.physical_catalog {
        let mut rows = Vec::new();
        crate::catalog::tuples::visit(
            source.as_ref(),
            uqa_sql::schema::constraint_metadata::CatalogOidClass::Constraint,
            &mut |_, payload| {
                if let Some(tuple) = ConstraintTuple::read_selected(
                    payload.get(),
                    request.includes("conbin"),
                    |fields| {
                        fields
                            .get("conname")
                            .and_then(|value| match value {
                                Value::Str(name) => Some(name),
                                _ => None,
                            })
                            .is_none_or(|name| request.matches_name("conname", name))
                    },
                )? {
                    rows.push(tuple.into_row());
                }
                Ok(())
            },
        )
        .map_err(|error| {
            uqa_sql::catalog::errors::storage_error("read physical constraint rows", &error)
        })?;
        for (relation, table) in &catalog.snapshot().tables {
            if table.persistence == uqa_sql::ast::RelationPersistence::Temporary {
                visit_table_constraint_tuples(
                    context,
                    catalog,
                    resolution,
                    relation,
                    table,
                    &mut |_, tuple| {
                        rows.push(tuple.into_row());
                        Ok(())
                    },
                )?;
            }
        }
        for (relation, table) in &*catalog.snapshot().definitions.foreign_tables {
            if table.persistence == uqa_sql::ast::RelationPersistence::Temporary {
                visit_foreign_constraint_tuples(
                    context,
                    catalog,
                    resolution,
                    relation,
                    table,
                    &mut |_, tuple| {
                        rows.push(tuple.into_row());
                        Ok(())
                    },
                )?;
            }
        }
        for (relation, triggers) in &*catalog.snapshot().definitions.triggers {
            if catalog
                .snapshot()
                .tables
                .get(relation)
                .is_some_and(|table| {
                    table.persistence == uqa_sql::ast::RelationPersistence::Temporary
                })
            {
                for trigger in triggers
                    .values()
                    .filter(|trigger| trigger.definition.constraint)
                {
                    rows.push(
                        super::super::events::trigger_constraint_tuple(
                            catalog, resolution, trigger,
                        )?
                        .into_row(),
                    );
                }
            }
        }
        return Ok(rows);
    }
    build_constraint_tuples(Some(context), catalog, resolution, request)
        .map(|tuples| tuples.into_iter().map(ConstraintTuple::into_row).collect())
}

/// Constraint identity inquiries do not construct expression payloads.
pub fn build_pg_constraint(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
) -> Result<Vec<ResultRow>, SQLError> {
    build_constraint_tuples(
        None,
        catalog,
        resolution,
        &super::super::CatalogRequest::columns([]),
    )
    .map(|tuples| tuples.into_iter().map(ConstraintTuple::into_row).collect())
}

fn build_constraint_tuples(
    context: Option<&crate::catalog::context::CatalogContext<'_>>,
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    request: &super::super::CatalogRequest,
) -> Result<Vec<ConstraintTuple>, SQLError> {
    let indexes = super::catalog_index_relations(catalog, resolution)?;
    let mut rows = constraint_catalog_rows(catalog, resolution)?
        .iter()
        .map(|constraint| -> Result<ConstraintTuple, SQLError> {
            constraint_tuple(
                context, catalog, resolution, request, constraint, indexes, None,
            )
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    rows.extend(super::constraint_definitions::domain_constraint_tuples(
        context, catalog, request,
    )?);
    rows.extend(super::super::events::build_trigger_constraint_tuples(
        catalog, resolution,
    )?);
    Ok(rows)
}

fn constraint_tuple(
    context: Option<&crate::catalog::context::CatalogContext<'_>>,
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    request: &super::super::CatalogRequest,
    constraint: &ConstraintCatalogRow,
    indexes: &[super::indexes::CatalogIndexRelation],
    table: Option<&crate::catalog::CatalogTableSnapshot>,
) -> Result<ConstraintTuple, SQLError> {
    let foreign_key = constraint.foreign_key.as_ref();
    let constrained_key = constraint_key(constraint)?;
    let constrained_relation_oid =
        constraint_relation_oid(catalog, resolution, &constraint.schema, &constraint.table)?;
    let referenced_relation_oid = match foreign_key {
        Some(foreign_key) => {
            constraint_relation_oid(catalog, resolution, &foreign_key.schema, &foreign_key.table)?
        }
        None => 0,
    };
    let parent_index_constraint_oid = constraint_parent_oid(catalog, constraint, indexes);
    let (inheritance_count, is_local) = if constraint.parent_oid.is_some() {
        (1, false)
    } else {
        constraint_inheritance_state(catalog, resolution, constraint, table)?
    };
    let fields = row([
        ("oid", int_value(constraint_row_oid(constraint))),
        ("conname", str_value(constraint.name.as_str())),
        (
            "connamespace",
            int_value(namespace_oid(catalog, &constraint.schema)),
        ),
        ("contype", str_value(constraint.kind.pg_type())),
        ("condeferrable", bool_value(constraint.state.deferrable())),
        (
            "condeferred",
            bool_value(constraint.state.initially_deferred()),
        ),
        ("conenforced", bool_value(constraint.state.enforced())),
        ("convalidated", bool_value(constraint.state.validated())),
        ("conrelid", int_value(constrained_relation_oid)),
        ("contypid", int_value(0)),
        (
            "conindid",
            int_value(constraint_index_oid(constraint, indexes)),
        ),
        ("conparentid", int_value(parent_index_constraint_oid)),
        ("confrelid", int_value(referenced_relation_oid)),
        (
            "confupdtype",
            str_value(foreign_key.map_or(" ", |foreign_key| {
                foreign_key_action_code(foreign_key.on_update)
            })),
        ),
        (
            "confdeltype",
            str_value(foreign_key.map_or(" ", |foreign_key| {
                foreign_key_action_code(foreign_key.on_delete)
            })),
        ),
        (
            "confmatchtype",
            str_value(foreign_key.map_or(" ", |foreign_key| {
                foreign_key_match_code(foreign_key.match_type)
            })),
        ),
        ("conislocal", bool_value(is_local)),
        ("coninhcount", int_value(inheritance_count)),
        ("connoinherit", bool_value(constraint.state.no_inherit())),
        ("conperiod", bool_value(constraint.period)),
        ("conkey", constrained_key),
        (
            "confkey",
            foreign_column_numbers(
                foreign_key.map(|key| key.column_ordinals.as_slice()),
                "pg_constraint.confkey",
            )?,
        ),
        (
            "confdelsetcols",
            foreign_column_numbers(
                foreign_key.map(|key| key.delete_set_ordinals.as_slice()),
                "pg_constraint.confdelsetcols",
            )?,
        ),
        ("conpfeqop", Value::Null),
        ("conppeqop", Value::Null),
        ("conffeqop", Value::Null),
        ("conexclop", Value::Null),
    ]);
    let expression = context
        .map(|context| {
            super::super::expression_nodes::constraint_node(
                context, catalog, constraint, request, table,
            )
        })
        .transpose()?
        .flatten();
    Ok(ConstraintTuple::new(fields, expression))
}

fn foreign_column_numbers(columns: Option<&[i64]>, field: &str) -> Result<Value, SQLError> {
    match columns.filter(|columns| !columns.is_empty()) {
        Some(columns) => catalog_array(columns.iter().copied().map(Value::Int).collect(), field),
        None => Ok(Value::Null),
    }
}

fn constraint_relation_oid(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    schema: &str,
    table: &str,
) -> Result<i64, SQLError> {
    table_relation_oid_from(
        catalog,
        resolution,
        &format!(
            "{}.{}",
            uqa_sql::expr::quote_ident(schema),
            uqa_sql::expr::quote_ident(table)
        ),
    )
}

/// Construct the selected relation without visiting unrelated table definitions.
pub(crate) fn visit_table_constraint_tuples(
    context: &crate::catalog::context::CatalogContext<'_>,
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    relation: &uqa_core::RelationIdentity,
    table: &crate::catalog::CatalogTableSnapshot,
    visitor: &mut dyn FnMut(
        uqa_core::catalog_identity::CatalogObjectIdentity,
        ConstraintTuple,
    ) -> Result<(), SQLError>,
) -> Result<(), SQLError> {
    let rows = super::super::helpers::constraints::table_constraint_catalog_rows(
        catalog,
        resolution,
        &relation.qualified_name(),
        table,
    )?;
    let has_index = rows.iter().any(|row| {
        matches!(
            row.kind,
            ConstraintCatalogKind::PrimaryKey
                | ConstraintCatalogKind::Unique { .. }
                | ConstraintCatalogKind::ForeignKey
        )
    });
    let indexes = if has_index {
        super::catalog_index_relations(catalog, resolution)?
    } else {
        &[]
    };
    for row in &rows {
        let identity = uqa_core::catalog_identity::CatalogObjectIdentity {
            oid: row
                .catalog_oid
                .ok_or_else(|| SQLError::Internal("constraint has no catalog OID".into()))?,
            object_id: row.object_id.ok_or_else(|| {
                SQLError::Internal("constraint has no catalog incarnation".into())
            })?,
        };
        visitor(
            identity,
            constraint_tuple(
                Some(context),
                catalog,
                resolution,
                &super::super::CatalogRequest::default(),
                row,
                indexes,
                Some(table),
            )?,
        )?;
    }
    Ok(())
}

pub(crate) fn visit_foreign_constraint_tuples(
    context: &crate::catalog::context::CatalogContext<'_>,
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    relation: &uqa_core::RelationIdentity,
    table: &crate::catalog::foreign::StoredForeignTable,
    visitor: &mut dyn FnMut(
        uqa_core::catalog_identity::CatalogObjectIdentity,
        ConstraintTuple,
    ) -> Result<(), SQLError>,
) -> Result<(), SQLError> {
    for row in super::super::helpers::constraints::foreign_constraint_catalog_rows(
        &relation.qualified_name(),
        table.clone(),
    )? {
        let identity = uqa_core::catalog_identity::CatalogObjectIdentity {
            oid: row
                .catalog_oid
                .ok_or_else(|| SQLError::Internal("constraint has no catalog OID".into()))?,
            object_id: row.object_id.ok_or_else(|| {
                SQLError::Internal("constraint has no catalog incarnation".into())
            })?,
        };
        visitor(
            identity,
            constraint_tuple(
                Some(context),
                catalog,
                resolution,
                &super::super::CatalogRequest::default(),
                &row,
                &[],
                None,
            )?,
        )?;
    }
    Ok(())
}

/// `conkey` includes the whole-row variable, unlike the named attribute dependency list.
fn constraint_key(constraint: &ConstraintCatalogRow) -> Result<Value, SQLError> {
    let key = if let (ConstraintCatalogKind::Check, Some(expression)) =
        (constraint.kind, &constraint.expression)
    {
        uqa_sql::schema::constraints::check_references(expression)
            .into_iter()
            .map(|reference| {
                reference
                    .attribute_number(|name| {
                        let column = constraint
                            .columns
                            .iter()
                            .find(|column| column.name == name)
                            .ok_or_else(|| {
                                SQLError::Internal(format!(
                                    "CHECK constraint `{}` references missing column `{name}`",
                                    constraint.name
                                ))
                            })?;
                        i16::try_from(column.table_ordinal).map_err(|_| {
                            SQLError::Internal("CHECK attribute number is out of range".into())
                        })
                    })
                    .map(|number| Value::Int(i64::from(number)))
            })
            .collect::<Result<Vec<_>, _>>()?
    } else {
        constraint
            .columns
            .iter()
            .map(|column| Value::Int(column.table_ordinal))
            .collect()
    };
    if key.is_empty() {
        Ok(Value::Null)
    } else {
        catalog_array(key, "pg_constraint.conkey")
    }
}

/// The `pg_constraint` OID of a relation constraint.
pub(crate) fn constraint_row_oid(constraint: &ConstraintCatalogRow) -> i64 {
    constraint.catalog_oid.unwrap_or_else(|| {
        constraint
            .object_id
            .filter(|_| constraint.kind == ConstraintCatalogKind::Check)
            .map_or_else(
                || {
                    stable_oid(
                        "constraint",
                        &format!(
                            "{}.{}.{}",
                            constraint.schema, constraint.table, constraint.name
                        ),
                    )
                },
                |object_id| stable_object_oid("constraint", &object_id),
            )
    })
}

fn constraint_inheritance_state(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    constraint: &ConstraintCatalogRow,
    selected: Option<&crate::catalog::CatalogTableSnapshot>,
) -> Result<(i64, bool), SQLError> {
    if constraint.kind == ConstraintCatalogKind::Check {
        return check_inheritance_state(catalog, resolution, constraint, selected);
    }
    if constraint.kind != ConstraintCatalogKind::NotNull {
        return Ok((0, true));
    }
    let column = constraint
        .columns
        .first()
        .ok_or_else(|| SQLError::Internal("NOT NULL constraint has no column".into()))?;
    let table_name = format!(
        "{}.{}",
        uqa_sql::expr::quote_ident(&constraint.schema),
        uqa_sql::expr::quote_ident(&constraint.table)
    );
    let retained = catalog.table(resolution, &table_name)?;
    let Some(table) = selected.or(retained) else {
        return Ok((0, true));
    };
    let is_local = table
        .columns
        .iter()
        .find(|definition| definition.name == column.name)
        .ok_or_else(|| SQLError::Internal("NOT NULL constraint column disappeared".into()))?
        .not_null_is_local;
    if constraint.state.no_inherit() {
        return Ok((0, is_local));
    }
    let mut count = 0;
    for parent in &table.hierarchy.parents {
        let parent_table = catalog
            .table(resolution, parent)?
            .ok_or_else(|| SQLError::UnknownTable(parent.clone()))?;
        if parent_table.columns.iter().any(|definition| {
            definition.name == column.name && definition.not_null && !definition.not_null_no_inherit
        }) {
            count += 1;
        }
    }
    Ok((
        catalog_usize(count, "pg_constraint NOT NULL inheritance count")?,
        is_local,
    ))
}

fn check_inheritance_state(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    constraint: &ConstraintCatalogRow,
    selected: Option<&crate::catalog::CatalogTableSnapshot>,
) -> Result<(i64, bool), SQLError> {
    let table_name = format!(
        "{}.{}",
        uqa_sql::expr::quote_ident(&constraint.schema),
        uqa_sql::expr::quote_ident(&constraint.table)
    );
    let retained = catalog.table(resolution, &table_name)?;
    let Some(table) = selected.or(retained) else {
        return Ok((0, true));
    };
    let is_local = table
        .columns
        .iter()
        .find(|column| {
            column.check.is_some() && column.check_name.as_ref() == Some(&constraint.name)
        })
        .map(|column| column.check_is_local)
        .or_else(|| {
            table
                .checks
                .iter()
                .find(|check| check.name.as_ref() == Some(&constraint.name))
                .map(|check| check.is_local)
        })
        .ok_or_else(|| SQLError::Internal("CHECK constraint disappeared".into()))?;
    let mut count = 0;
    if !constraint.state.no_inherit() {
        for parent in &table.hierarchy.parents {
            let parent = catalog
                .table(resolution, parent)?
                .ok_or_else(|| SQLError::UnknownTable(parent.clone()))?;
            if parent.columns.iter().any(|column| {
                column.check.is_some()
                    && !column.check_no_inherit
                    && column.check_name.as_ref() == Some(&constraint.name)
            }) || parent
                .checks
                .iter()
                .any(|check| !check.no_inherit && check.name.as_ref() == Some(&constraint.name))
            {
                count += 1;
            }
        }
    }
    Ok((
        catalog_usize(count, "pg_constraint CHECK inheritance count")?,
        is_local,
    ))
}

const fn foreign_key_action_code(action: uqa_sql::ast::ForeignKeyAction) -> &'static str {
    match action {
        uqa_sql::ast::ForeignKeyAction::NoAction => "a",
        uqa_sql::ast::ForeignKeyAction::Restrict => "r",
        uqa_sql::ast::ForeignKeyAction::Cascade => "c",
        uqa_sql::ast::ForeignKeyAction::SetNull => "n",
        uqa_sql::ast::ForeignKeyAction::SetDefault => "d",
    }
}

const fn foreign_key_match_code(match_type: uqa_sql::ast::ForeignKeyMatch) -> &'static str {
    match match_type {
        uqa_sql::ast::ForeignKeyMatch::Simple => "s",
        uqa_sql::ast::ForeignKeyMatch::Full => "f",
    }
}

/// `conindid`: the index that implements a key constraint, or the unique index a foreign key references; zero for other constraints.
pub(crate) fn constraint_index_oid(
    constraint: &super::super::helpers::constraints::ConstraintCatalogRow,
    indexes: &[super::CatalogIndexRelation],
) -> i64 {
    use super::super::helpers::constraints::ConstraintCatalogKind;
    indexes
        .iter()
        .find(|index| {
            if let Some(foreign) = &constraint.foreign_key {
                foreign.referenced_index.is_some_and(|id| {
                    index
                        .definition
                        .catalog
                        .as_ref()
                        .is_some_and(|identity| identity.identity.object_id == id)
                })
            } else if matches!(
                constraint.kind,
                ConstraintCatalogKind::PrimaryKey | ConstraintCatalogKind::Unique { .. }
            ) {
                constraint.object_id.is_some()
                    && index.definition.relationships.owning_constraint == constraint.object_id
            } else {
                false
            }
        })
        .map_or(0, super::CatalogIndexRelation::oid)
}

/// `conparentid`: the foreign key that a constraint derived on a referenced partition belongs to, or the constraint of the parent partitioned table whose index is the parent of this key constraint's index; zero otherwise.
pub(crate) fn constraint_parent_oid(
    catalog: &CatalogReadView,
    constraint: &ConstraintCatalogRow,
    indexes: &[super::CatalogIndexRelation],
) -> i64 {
    if let Some(parent) = constraint.parent_oid {
        return parent;
    }
    let Some(owner) = constraint.object_id else {
        return 0;
    };
    let Some(parent) = indexes
        .iter()
        .find(|index| index.definition.relationships.owning_constraint == Some(owner))
        .and_then(|index| index.definition.relationships.parent_index)
    else {
        return 0;
    };
    let Some(owner) = indexes
        .iter()
        .find(|index| {
            index
                .definition
                .catalog
                .as_ref()
                .is_some_and(|id| id.identity.object_id == parent)
        })
        .and_then(|index| index.definition.relationships.owning_constraint)
    else {
        return 0;
    };
    catalog
        .snapshot()
        .tables
        .values()
        .flat_map(|table| table.keys.iter())
        .filter_map(|key| key.catalog_identity)
        .find(|identity| identity.object_id == owner)
        .map_or(0, |identity| identity.oid)
}
