//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored routine dependencies on deleted columns and their owned objects.

use super::{BTreeMap, BTreeSet, Engine, RoutineDropResolution, SQLError, SQLFunctionDropPlan};

impl Engine {
    pub(crate) fn drop_column_routine_dependents(
        &self,
        table: &str,
        column: &str,
        cascade: bool,
    ) -> Result<(), SQLError> {
        let registry = self.durable.sql_user_functions.read().clone();
        if registry.is_empty() {
            return Ok(());
        }
        let mut resolution = RoutineDropResolution {
            targets: Vec::new(),
            seen_targets: BTreeSet::new(),
            notices: Vec::new(),
        };
        let mut domains = BTreeSet::new();
        self.expand_routine_domain_column_drop(
            &registry,
            &mut resolution,
            &mut domains,
            BTreeSet::from([(table.to_string(), column.to_string())]),
        )?;
        if resolution.targets.is_empty() {
            return Ok(());
        }
        if !cascade {
            let oid = crate::sql::resolve_bound_regclass_oid(self, table)?
                .ok_or_else(|| SQLError::Internal("DROP COLUMN table disappeared".into()))?;
            let label =
                crate::sql::resolve_regtype_output(self, &uqa_sql::ast::ColumnType::Regclass, oid)
                    .map_err(SQLError::Internal)?
                    .unwrap_or_else(|| table.to_string());
            return Err(SQLError::Routine {
                sqlstate: "2BP01".into(),
                message: format!(
                    "cannot drop column {column} of table {label} because other objects depend on it"
                ),
            });
        }
        let dependents = self.routine_object_dependents(&resolution.targets, true)?;
        self.commit_sql_function_drop(SQLFunctionDropPlan {
            domains,
            targets: resolution.targets,
            dependents,
            notices: resolution.notices,
        })
    }

    pub(super) fn expand_column_drop_dependencies(
        &self,
        columns: &mut BTreeSet<(String, String)>,
        relations: &mut BTreeSet<String>,
    ) -> Result<(), SQLError> {
        if columns.is_empty() {
            return Ok(());
        }
        let mut tables = self
            .table_entries()
            .into_iter()
            .map(|(name, table)| (name, (table.object_id(), table.columns.read().clone())))
            .collect::<BTreeMap<_, _>>();
        tables.extend(
            self.durable
                .foreign_tables
                .read()
                .iter()
                .map(|(identity, table)| {
                    (
                        identity.qualified_name(),
                        (table.object_id, table.columns.clone()),
                    )
                }),
        );
        let mut pending = columns.iter().cloned().collect::<Vec<_>>();
        while let Some((table, column)) = pending.pop() {
            let (table_id, definitions) = tables.get(&table).ok_or_else(|| {
                SQLError::Internal(format!("dependent table {table} disappeared"))
            })?;
            let column_id = definitions
                .iter()
                .find(|definition| definition.name == column)
                .and_then(|definition| definition.object_id)
                .ok_or_else(|| {
                    SQLError::Internal(format!("dependent column {table}.{column} has no identity"))
                })?;
            relations.extend(
                self.sequence_names_owned_by_column(*table_id, column_id)
                    .map_err(|error| {
                        SQLError::Internal(format!("inspect column sequences: {error}"))
                    })?,
            );
            relations.extend(
                self.views_depending_on_column(&table, &column)
                    .map_err(|error| {
                        SQLError::Internal(format!("inspect column views: {error}"))
                    })?,
            );
            for definition in definitions {
                if definition.generated.as_ref().is_some_and(|generated| {
                    crate::engine_table_storage::schema_expr_references_column(
                        &generated.expression,
                        &column,
                    )
                }) {
                    let dependent = (table.clone(), definition.name.clone());
                    if columns.insert(dependent.clone()) {
                        pending.push(dependent);
                    }
                }
            }
        }
        Ok(())
    }
}
