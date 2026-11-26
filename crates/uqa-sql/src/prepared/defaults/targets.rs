//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Select the physical columns whose defaults appear in a rewritten mutation.

use crate::{
    ast::AssignmentTarget,
    plan::{AssignmentPlan, CommandPlan, ConflictActionPlan, MergeWhenPlan},
    semantics::view_rewrite::production::OwnedAutomaticViewLayer,
    SQLError, ScalarExpr,
};

pub(super) struct TargetDefaults<'a> {
    command: &'a CommandPlan,
    layers: &'a [OwnedAutomaticViewLayer],
    insert_width: Option<usize>,
}

impl<'a> TargetDefaults<'a> {
    pub(super) fn new(
        command: &'a CommandPlan,
        layers: &'a [OwnedAutomaticViewLayer],
    ) -> Result<Self, SQLError> {
        let insert_width = match command {
            CommandPlan::Insert(plan) if plan.columns.is_empty() => plan
                .source
                .as_deref()
                .map(|source| {
                    let schema = source.analyzed_output.as_ref().ok_or_else(|| {
                        SQLError::Internal("prepared INSERT source has no analyzed output".into())
                    })?;
                    let width = schema.len();
                    plan.rule_values_input
                        .as_ref()
                        .map_or(Ok(width), |values| values.value_width(width))
                })
                .transpose()?,
            _ => None,
        };
        Ok(Self {
            command,
            layers,
            insert_width,
        })
    }

    fn physical_name<'b>(&'b self, mut name: &'b str) -> Option<&'b str> {
        for layer in self.layers {
            let projected = layer
                .columns
                .iter()
                .find(|column| column.name.as_str() == name)?;
            name = projected.writable_source_column.as_deref()?.as_str();
        }
        Some(name)
    }

    fn position(
        &self,
        targets: &[AssignmentTarget<ScalarExpr>],
        name: &str,
        index: usize,
    ) -> Option<usize> {
        if !targets.is_empty() {
            return targets
                .iter()
                .position(|target| self.physical_name(&target.column) == Some(name));
        }
        self.layers.first().map_or(Some(index), |layer| {
            layer
                .columns
                .iter()
                .position(|column| self.physical_name(&column.name) == Some(name))
        })
    }

    fn assignment_default(&self, assignments: &[AssignmentPlan], name: &str) -> bool {
        assignments.iter().any(|assignment| {
            assignment
                .target
                .targets()
                .iter()
                .enumerate()
                .any(|(position, target)| {
                    if self.physical_name(&target.column) != Some(name) {
                        return false;
                    }
                    match &assignment.value {
                        ScalarExpr::Default => true,
                        ScalarExpr::Row(values) => matches!(
                            values.get(assignment.target.source_position(position)),
                            Some(ScalarExpr::Default)
                        ),
                        _ => false,
                    }
                })
        })
    }

    pub(super) fn needs(&self, name: &str, index: usize) -> bool {
        match self.command {
            CommandPlan::Insert(plan) => {
                let position = self.position(&plan.columns, name, index);
                let inserted = position.is_none_or(|position| {
                    if plan.source.is_some() {
                        return plan.columns.is_empty()
                            && self.insert_width.is_some_and(|width| position >= width);
                    }
                    plan.rows.is_empty()
                        || plan.rows.iter().any(|row| {
                            matches!(row.get(position), None | Some(ScalarExpr::Default))
                        })
                });
                inserted || plan.on_conflict.as_ref().is_some_and(|conflict| {
                    matches!(&conflict.action, ConflictActionPlan::Update { assignments, .. } if self.assignment_default(assignments, name))
                })
            }
            CommandPlan::Update(plan) => self.assignment_default(&plan.assignments, name),
            CommandPlan::Merge(plan) => plan.when_clauses.iter().any(|clause| match clause {
                MergeWhenPlan::UpdateMatched { assignments, .. }
                | MergeWhenPlan::UpdateNotMatchedBySource { assignments, .. } => {
                    self.assignment_default(assignments, name)
                }
                MergeWhenPlan::InsertNotMatched {
                    columns, values, ..
                } => self.position(columns, name, index).is_none_or(|position| {
                    matches!(values.get(position), None | Some(ScalarExpr::Default))
                }),
                _ => false,
            }),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_selected_domain_defaults_enter_prepared_dependencies() {
        let cases = [
            ("INSERT INTO t(a,b) VALUES(1,DEFAULT),(2,3)", [false, true, true]),
            ("INSERT INTO t VALUES (1,2)", [false, false, true]),
            ("INSERT INTO t(a,b,c) VALUES (1,2,3) ON CONFLICT(a) DO UPDATE SET c=DEFAULT", [false, false, true]),
            ("UPDATE t SET a=1,b=DEFAULT", [false, true, false]),
            ("MERGE INTO t USING s ON (t.a=s.a) WHEN MATCHED THEN UPDATE SET c=DEFAULT WHEN NOT MATCHED THEN INSERT(a,c) VALUES (1,2)", [false, true, true]),
        ];
        for (sql, expected) in cases {
            let crate::plan::UnifiedPlan::Command(command) =
                crate::plan::UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
            else {
                panic!("expected a write")
            };
            let selected = TargetDefaults::new(&command, &[]).unwrap();
            assert_eq!(
                [
                    selected.needs("a", 0),
                    selected.needs("b", 1),
                    selected.needs("c", 2)
                ],
                expected,
                "{sql}"
            );
        }
    }
}
