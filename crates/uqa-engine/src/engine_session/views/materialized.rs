//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Materialized-view creation, snapshot replacement, and refresh lifecycle.

use super::{
    create_view_output_columns, Engine, MaterializedViewRegistration, RelationIdentity, SQLError,
    StoredView, StoredViewKind, ViewRow,
};

fn materialized_rows(
    result: &uqa_sql::SQLResult,
    output_columns: &[String],
) -> Result<Vec<uqa_sql::ResultRow>, SQLError> {
    if result.columns.len() != output_columns.len() {
        return Err(SQLError::Internal(format!(
            "materialized-view query schema width {} changed to {} during execution",
            output_columns.len(),
            result.columns.len()
        )));
    }
    result
        .rows
        .iter()
        .enumerate()
        .map(|(row_index, _)| {
            output_columns
                .iter()
                .enumerate()
                .map(|(column_index, column)| {
                    result
                        .value_at(row_index, column_index)
                        .cloned()
                        .map(|value| (column.clone(), value))
                        .ok_or_else(|| {
                            SQLError::Internal(format!(
                                "materialized-view row {row_index} is missing column {column_index}"
                            ))
                        })
                })
                .collect()
        })
        .collect()
}

impl Engine {
    pub(crate) fn register_materialized_view_plan(
        &self,
        registration: MaterializedViewRegistration<'_>,
    ) -> Result<(), SQLError> {
        let MaterializedViewRegistration {
            name,
            column_names,
            mut plan,
            if_not_exists,
            with_no_data,
            options,
            params,
        } = registration;
        self.with_implicit_transaction(move |engine| {
            engine.synchronize_catalog_registries().map_err(|error| {
                SQLError::Internal(format!("refresh materialized-view catalog: {error}"))
            })?;
            let name = engine
                .try_relation_name_for_create(name)
                .map_err(SQLError::Unsupported)?;
            if let Some(kind) = engine.relation_kind_at(&name).map_err(|error| {
                SQLError::Internal(format!("resolve relation `{name}`: {error}"))
            })? {
                if if_not_exists {
                    return Ok(());
                }
                return Err(SQLError::Routine {
                    sqlstate: "42P07".into(),
                    message: format!("relation \"{name}\" already exists as {kind}"),
                });
            }
            if engine.bind_view_plan_for_create(&mut plan)? {
                return Err(SQLError::Routine {
                    sqlstate: "0A000".into(),
                    message: "materialized views must not use temporary tables or views".into(),
                });
            }
            let query_schema = crate::sql::bind_catalog_query_routines(engine, &mut plan, params)?;
            let output_columns = create_view_output_columns(&query_schema, column_names)?;
            let materialized_column_types = query_schema.column_types().to_vec();
            let materialized_rows = if with_no_data {
                Vec::new()
            } else {
                let result = crate::sql::execute_query_plan(engine, &plan, params)?;
                materialized_rows(&result, &output_columns)?
            };
            let relation = RelationIdentity::from_legacy_name(&name).map_err(|error| {
                SQLError::Internal(format!("invalid materialized-view name: {error}"))
            })?;
            let view = StoredView {
                query: plan,
                output_columns: Some(output_columns),
                persistence: uqa_sql::ast::RelationPersistence::Permanent,
                options: options.to_vec(),
                kind: StoredViewKind::Materialized,
                materialized_rows,
                materialized_column_types,
                populated: !with_no_data,
            };
            if let Some(catalog) = engine.storage.catalog.as_ref() {
                let definition_json = serde_json::to_string(&view).map_err(|error| {
                    SQLError::Internal(format!("serialize materialized view `{name}`: {error}"))
                })?;
                catalog
                    .save_view(&ViewRow {
                        relation: relation.clone(),
                        definition_json,
                    })
                    .map_err(|error| {
                        SQLError::Internal(format!("persist materialized view `{name}`: {error}"))
                    })?;
            }
            engine.durable.views.write().insert(relation, view);
            engine.note_catalog_registry_changed();
            Ok(())
        })
    }

    pub(crate) fn refresh_materialized_view(
        &self,
        name: &str,
        concurrently: bool,
        with_no_data: bool,
    ) -> Result<(), SQLError> {
        if concurrently {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: "REFRESH MATERIALIZED VIEW CONCURRENTLY requires a qualifying unique index, which is not available".into(),
            });
        }
        self.with_implicit_transaction(move |engine| {
            let (canonical, kind) = engine
                .try_resolve_relation_kind(name)
                .map_err(|error| {
                    SQLError::Internal(format!("resolve materialized view `{name}`: {error}"))
                })?
                .ok_or_else(|| SQLError::Routine {
                    sqlstate: "42P01".into(),
                    message: format!("relation \"{name}\" does not exist"),
                })?;
            if kind != "materialized view" {
                return Err(SQLError::Routine {
                    sqlstate: "0A000".into(),
                    message: format!("\"{name}\" is not a materialized view"),
                });
            }
            let relation = RelationIdentity::from_legacy_name(&canonical).map_err(|error| {
                SQLError::Internal(format!("invalid materialized-view name: {error}"))
            })?;
            let mut view = engine
                .durable
                .views
                .read()
                .get(&relation)
                .cloned()
                .ok_or_else(|| {
                    SQLError::Internal(format!("materialized view `{canonical}` disappeared"))
                })?;
            view.materialized_rows = if with_no_data {
                Vec::new()
            } else {
                let result = crate::sql::execute_query_plan(engine, &view.query, &[])?;
                let rows = materialized_rows(
                    &result,
                    view.output_columns.as_deref().unwrap_or(&result.columns),
                )?;
                view.materialized_column_types = result.column_types;
                rows
            };
            view.populated = !with_no_data;
            if let Some(catalog) = engine.storage.catalog.as_ref() {
                let definition_json = serde_json::to_string(&view).map_err(|error| {
                    SQLError::Internal(format!(
                        "serialize materialized view `{canonical}`: {error}"
                    ))
                })?;
                catalog
                    .save_view(&ViewRow {
                        relation: relation.clone(),
                        definition_json,
                    })
                    .map_err(|error| {
                        SQLError::Internal(format!(
                            "persist materialized view `{canonical}`: {error}"
                        ))
                    })?;
            }
            engine.durable.views.write().insert(relation, view);
            engine.note_catalog_registry_changed();
            Ok(())
        })
    }
}
