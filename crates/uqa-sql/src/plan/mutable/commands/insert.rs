//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! INSERT input edits retain the command's query box and move VALUES payloads into their new relational owner.

use super::{CommandPlan, MutableCommandPlan, MutableQueryPlan, RewriteMemory, SQLError};
use crate::plan::{copy::Copying, InsertPlan, QueryPlan, RelationalPlan, RuleValuesInput};
use uqa_core::memory::{BudgetedVec, Produced, ProductionControl};

impl InsertPlan {
    /// Copy one rule relation without a source or nested view schedule. The first VALUES layer alone retains rows; later layers receive an independently owned input query.
    pub fn copy_rule_layer_for_rewrite(
        &self,
        layer: Option<&crate::plan::ViewRuleInsertPlan>,
        rows: bool,
        control: &ProductionControl<'_>,
    ) -> Result<MutableCommandPlan, SQLError> {
        control.check()?;
        if let Some(mut copying) = Copying::for_rewrite(*control) {
            let command = copying.insert_rule_layer_command(self, layer, rows)?;
            return Ok(MutableCommandPlan::from_rewrite_parts(
                command,
                Some(copying.into_rewrite_memory()),
            ));
        }
        let mut plan = self.clone();
        plan.source = None;
        if !rows {
            plan.rows.clear();
        }
        plan.view_rule_insert_plans.clear();
        plan.view_rule_relations.clear();
        if let Some(layer) = layer {
            plan.table.clone_from(&layer.relation);
            plan.target_qualifier.clone_from(&layer.relation);
            plan.columns = layer
                .supplied_columns
                .iter()
                .zip(&self.columns)
                .map(|(name, target)| {
                    let mut target = target.clone();
                    target.column.clone_from(name);
                    target
                })
                .collect();
            plan.returning.clear();
            if let Some(returning) = self
                .view_rule_returning
                .as_ref()
                .filter(|returning| returning.relation == layer.relation)
            {
                plan.target_qualifier
                    .clone_from(&returning.target_qualifier);
                plan.returning.clone_from(&returning.returning);
                plan.returning_aliases.clone_from(&returning.aliases);
                plan.subqueries.clone_from(&returning.subqueries);
            }
        }
        Ok(MutableCommandPlan::from_rewrite_parts(
            CommandPlan::Insert(Box::new(plan)),
            None,
        ))
    }
}

impl MutableCommandPlan {
    pub fn map_insert_source(
        mut self,
        control: &ProductionControl<'_>,
        map: impl FnOnce(&InsertPlan, Option<MutableQueryPlan>) -> Result<MutableQueryPlan, SQLError>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let CommandPlan::Insert(plan) = &mut self.command else {
            return Err(SQLError::Internal(
                "INSERT source edit requires INSERT".into(),
            ));
        };
        let source = plan.source.as_mut().map(|query| MutableQueryPlan {
            query: std::mem::replace(query.as_mut(), super::super::queries::empty_query()),
            memory: self.memory.as_mut().and_then(|memory| {
                body(memory)
                    .field_mut("source")
                    .as_mut()
                    .expect("INSERT source box")
                    .field_mut("query")
                    .take()
            }),
        });
        let query = map(plan, source)?;
        query.assert_owner(control);
        control.check()?;
        if let Some(source) = &mut plan.source {
            **source = query.query;
            if let Some(memory) = &mut self.memory {
                *body(memory)
                    .field_mut("source")
                    .as_mut()
                    .expect("INSERT source box")
                    .field_mut("query") = query.memory;
            }
        } else {
            let (source, memory) = if let Some(mut copying) = Copying::for_rewrite(*control) {
                let source = copying.boxed_query(query)?;
                (source, Some(copying.into_rewrite_memory()))
            } else {
                (Box::new(query.query), None)
            };
            plan.source = Some(source);
            if let Some(owner) = &mut self.memory {
                *body(owner).field_mut("source") = memory;
            }
        }
        Ok(self)
    }

    pub fn set_implicit_insert_columns<'a>(
        mut self,
        names: impl ExactSizeIterator<Item = &'a str>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let CommandPlan::Insert(plan) = &mut self.command else {
            return Err(SQLError::Internal("implicit columns require INSERT".into()));
        };
        if !plan.columns.is_empty() {
            return Ok(self);
        }
        let (columns, memory) = if let Some(mut copying) = Copying::for_rewrite(*control) {
            let columns = copying.implicit_insert_columns(names)?;
            (columns, Some(copying.into_rewrite_memory()))
        } else {
            (
                names
                    .map(|name| crate::ast::AssignmentTarget::from(name.to_owned()))
                    .collect(),
                None,
            )
        };
        plan.columns = columns;
        if let Some(owner) = &mut self.memory {
            *body(owner).field_mut("columns") = memory;
        }
        Ok(self)
    }

    pub fn set_rule_values_input(
        mut self,
        input: Produced<RuleValuesInput>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let CommandPlan::Insert(plan) = &mut self.command else {
            return Err(SQLError::Internal(
                "VALUES provenance requires INSERT".into(),
            ));
        };
        let (input, memory) = input.into_parts();
        let (input, memory) = control.finish(input, memory)?.into_parts();
        plan.rule_values_input = Some(input);
        if let Some(owner) = &mut self.memory {
            *body(owner).field_mut("rule_values_input") =
                memory.map(|direct| RewriteMemory::Fields {
                    fields: BudgetedVec::new(direct.budget()),
                    direct,
                });
        }
        Ok(self)
    }

    /// VALUES leave the command without a payload copy. The command retains its own scalar-query namespace for RETURNING and assignment targets; the new query admits its independent namespace copy.
    pub fn take_insert_rows_query(
        mut self,
        control: &ProductionControl<'_>,
    ) -> Result<(Self, MutableQueryPlan), SQLError> {
        self.assert_owner(control);
        control.check()?;
        let CommandPlan::Insert(plan) = &mut self.command else {
            return Err(SQLError::Internal(
                "VALUES extraction requires INSERT".into(),
            ));
        };
        let rows = super::super::values::OwnedRows {
            rows: std::mem::take(&mut plan.rows),
            memory: self
                .memory
                .as_mut()
                .and_then(|memory| body(memory).field_mut("rows").take()),
        };
        let query = if let Some(mut copying) = Copying::for_rewrite(*control) {
            let query =
                copying.insert_values_query(rows, &plan.subqueries, plan.relations_bound)?;
            MutableQueryPlan::from_rewrite_parts(query, Some(copying.into_rewrite_memory()))
        } else {
            MutableQueryPlan::from_rewrite_parts(
                QueryPlan {
                    analyzed_output: None,
                    analyzed_source: None,
                    analyzed_grouping: None,
                    relation_privilege_subject: None,
                    relations_bound: plan.relations_bound,
                    ctes: Vec::new(),
                    root: RelationalPlan::Values {
                        rows: rows.rows,
                        subqueries: plan.subqueries.clone(),
                    },
                },
                None,
            )
        };
        Ok((self, query))
    }
}

fn body(memory: &mut RewriteMemory) -> &mut RewriteMemory {
    memory.field_mut("value0").as_mut().expect("INSERT body")
}
