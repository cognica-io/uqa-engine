//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persistent MERGE write-target identities across column deletion and reuse.

use uqa_sql::ast::{
    ColumnDef, ColumnType, Expr, MergeStmt, MergeTargetColumnBinding, MergeWhen, Statement,
};

use super::{BTreeMap, BTreeSet, Engine, SQLError};

impl Engine {
    fn stored_merge_target_definitions(&self, table: &str) -> Option<Vec<ColumnDef>> {
        self.table_entries()
            .into_iter()
            .find(|(name, _)| name == table)
            .map(|(_, table)| table.columns.read().clone())
    }

    pub(super) fn bind_stored_merge_target_columns(
        &self,
        statement: &mut Statement,
    ) -> Result<bool, SQLError> {
        let mut changed = false;
        crate::events::visit_stored_statement_merges(statement, &mut |merge| {
            let Some(definitions) = self.stored_merge_target_definitions(&merge.target) else {
                return Ok(());
            };
            let previous = merge.target_column_bindings.clone();
            let mut targets = BTreeSet::new();
            let mut coerced_targets = BTreeSet::new();
            for action in &mut merge.when_clauses {
                match action {
                    MergeWhen::UpdateMatched { assignments, .. }
                    | MergeWhen::UpdateNotMatchedBySource { assignments, .. } => {
                        targets.extend(assignments.iter().map(|(name, _)| name.clone()));
                        coerced_targets.extend(
                            assignments
                                .iter()
                                .filter(|(_, expression)| !matches!(expression, Expr::Default))
                                .map(|(name, _)| name.clone()),
                        );
                    }
                    MergeWhen::InsertNotMatched {
                        columns, values, ..
                    } => {
                        if columns.is_empty() && !values.is_empty() {
                            *columns = definitions
                                .iter()
                                .take(values.len())
                                .map(|column| column.name.clone())
                                .collect();
                            changed = true;
                        }
                        targets.extend(columns.iter().cloned());
                        coerced_targets.extend(
                            columns
                                .iter()
                                .zip(values.iter())
                                .filter(|(_, expression)| !matches!(expression, Expr::Default))
                                .map(|(name, _)| name.clone()),
                        );
                    }
                    _ => {}
                }
            }
            for name in targets {
                if let Some(column) = definitions.iter().find(|column| column.name == name) {
                    if let Some(object_id) = column.object_id {
                        let mut domain_dependencies = BTreeSet::new();
                        if coerced_targets.contains(&name) {
                            collect_target_domains(&column.ty, &mut domain_dependencies);
                        }
                        merge.target_column_bindings.entry(name).or_insert(
                            MergeTargetColumnBinding {
                                object_id,
                                domain_dependencies,
                            },
                        );
                    }
                }
            }
            changed |= merge.target_column_bindings != previous;
            Ok(())
        })?;
        Ok(changed)
    }

    pub(crate) fn dropped_stored_merge_targets(&self, merge: &MergeStmt) -> BTreeSet<String> {
        if merge.target_column_bindings.is_empty() {
            return BTreeSet::new();
        }
        let live = self
            .stored_merge_target_definitions(&merge.target)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|column| column.object_id)
            .collect::<BTreeSet<_>>();
        merge
            .target_column_bindings
            .iter()
            .filter(|(_, binding)| !live.contains(&binding.object_id))
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// Normalize only a compilation copy. The durable body retains expression and routine/sequence dependencies on retired write targets.
    pub(crate) fn normalize_stored_merge_target_columns(
        &self,
        statement: &mut Statement,
    ) -> Result<(), SQLError> {
        crate::events::visit_stored_statement_merges(statement, &mut |merge| {
            if self.dropped_stored_merge_targets(merge).is_empty() {
                return Ok(());
            }
            let current = self
                .stored_merge_target_definitions(&merge.target)
                .unwrap_or_default()
                .into_iter()
                .filter_map(|column| column.object_id.map(|id| (id, column.name)))
                .collect::<BTreeMap<_, _>>();
            let name_for = |name: &str| match merge.target_column_bindings.get(name) {
                Some(binding) => current.get(&binding.object_id).cloned(),
                None => Some(name.to_string()),
            };
            for action in &mut merge.when_clauses {
                match action {
                    MergeWhen::UpdateMatched { assignments, .. }
                    | MergeWhen::UpdateNotMatchedBySource { assignments, .. } => {
                        assignments.retain_mut(|(name, _)| {
                            if let Some(current) = name_for(name) {
                                *name = current;
                                true
                            } else {
                                false
                            }
                        });
                    }
                    MergeWhen::InsertNotMatched {
                        columns, values, ..
                    } => {
                        let mut surviving = Vec::new();
                        let mut expressions = Vec::new();
                        for (column, expression) in std::mem::take(columns)
                            .into_iter()
                            .zip(std::mem::take(values))
                        {
                            if let Some(current) = name_for(&column) {
                                surviving.push(current);
                                expressions.push(expression);
                            }
                        }
                        *columns = surviving;
                        *values = expressions;
                    }
                    _ => {}
                }
            }
            Ok(())
        })
    }

    pub(crate) fn refresh_stored_merge_target_plans(&self) -> Result<(), SQLError> {
        self.rewrite_stored_routine_bodies(|statement| {
            let mut changed = false;
            crate::events::visit_stored_statement_merges(statement, &mut |merge| {
                changed |= !self.dropped_stored_merge_targets(merge).is_empty();
                Ok(())
            })?;
            Ok(changed)
        })
    }
}

fn collect_target_domains(ty: &ColumnType, dependencies: &mut BTreeSet<u32>) {
    match ty {
        ColumnType::Domain { oid, base, .. } => {
            dependencies.insert(*oid);
            collect_target_domains(base, dependencies);
        }
        ColumnType::Array(element) => collect_target_domains(element, dependencies),
        _ => {}
    }
}
