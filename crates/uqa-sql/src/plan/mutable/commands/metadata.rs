//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Mutation metadata replacements retire only their own constructor fields.

use super::{CommandPlan, MutableCommandPlan, RewriteMemory, SQLError};
use crate::{catalog::roles::RoleReference, plan::copy::Copying};
use uqa_core::memory::{BudgetedVec, Produced, ProductionControl};

impl MutableCommandPlan {
    pub fn set_target_privilege_subject(
        mut self,
        subject: Produced<RoleReference>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let (subject, owner) = field_owner(subject, control)?;
        let target = match &mut self.command {
            CommandPlan::Insert(plan) => &mut plan.target_privilege_subject,
            CommandPlan::Update(plan) => &mut plan.target_privilege_subject,
            CommandPlan::Delete(plan) => &mut plan.target_privilege_subject,
            CommandPlan::Merge(plan) => &mut plan.target_privilege_subject,
            _ => {
                return Err(SQLError::Internal(
                    "target authority requires a mutation".into(),
                ))
            }
        };
        *target = Some(subject);
        if let Some(memory) = &mut self.memory {
            *body(memory).field_mut("target_privilege_subject") = owner;
        }
        Ok(self)
    }

    pub fn retarget_mutation(
        mut self,
        target: Produced<String>,
        include_descendants: bool,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let (target, owner) = field_owner(target, control)?;
        let (name, descendants, field) = match &mut self.command {
            CommandPlan::Insert(plan) => (&mut plan.table, &mut plan.include_descendants, "table"),
            CommandPlan::Update(plan) => (&mut plan.table, &mut plan.include_descendants, "table"),
            CommandPlan::Delete(plan) => (&mut plan.table, &mut plan.include_descendants, "table"),
            CommandPlan::Merge(plan) => (&mut plan.target, &mut plan.include_descendants, "target"),
            _ => return Err(SQLError::Internal("retargeting requires a mutation".into())),
        };
        *name = target;
        *descendants = include_descendants;
        if let Some(memory) = &mut self.memory {
            *body(memory).field_mut(field) = owner;
        }
        Ok(self)
    }

    pub fn clear_mutation_ctes(
        mut self,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let ctes = match &mut self.command {
            CommandPlan::Insert(plan) => &mut plan.ctes,
            CommandPlan::Update(plan) => &mut plan.ctes,
            CommandPlan::Delete(plan) => &mut plan.ctes,
            CommandPlan::Merge(plan) => &mut plan.ctes,
            _ => {
                return Err(SQLError::Internal(
                    "CTE retirement requires a mutation".into(),
                ))
            }
        };
        ctes.clear();
        if let Some(memory) = &mut self.memory {
            let RewriteMemory::Fields { fields, .. } =
                body(memory).field_mut("ctes").as_mut().expect("CTE vector")
            else {
                unreachable!("CTE vector constructor")
            };
            fields.clear();
        }
        Ok(self)
    }

    pub fn record_view_rule_relation(
        mut self,
        relation: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let relations = match &mut self.command {
            CommandPlan::Insert(plan) => &mut plan.view_rule_relations,
            CommandPlan::Update(plan) => &mut plan.view_rule_relations,
            CommandPlan::Delete(plan) => &mut plan.view_rule_relations,
            _ => {
                return Err(SQLError::Internal(
                    "view rules require INSERT, UPDATE or DELETE".into(),
                ))
            }
        };
        if relations.iter().any(|name| name == relation) {
            return Ok(self);
        }
        let old = std::mem::take(relations);
        let memory = self
            .memory
            .as_mut()
            .and_then(|memory| body(memory).field_mut("view_rule_relations").take());
        let (names, owner) = append_rule_relation(old, memory, relation, control)?;
        let relations = match &mut self.command {
            CommandPlan::Insert(plan) => &mut plan.view_rule_relations,
            CommandPlan::Update(plan) => &mut plan.view_rule_relations,
            CommandPlan::Delete(plan) => &mut plan.view_rule_relations,
            _ => unreachable!(),
        };
        *relations = names;
        if let Some(memory) = &mut self.memory {
            *body(memory).field_mut("view_rule_relations") = owner;
        }
        Ok(self)
    }

    pub fn preserve_view_rule_returning(
        mut self,
        relation: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let (retained, qualifier, projections, aliases, queries) = match &mut self.command {
            CommandPlan::Insert(plan) => (
                &mut plan.view_rule_returning,
                &plan.target_qualifier,
                &plan.returning,
                &plan.returning_aliases,
                &plan.subqueries,
            ),
            CommandPlan::Update(plan) => (
                &mut plan.view_rule_returning,
                &plan.target_qualifier,
                &plan.returning,
                &plan.returning_aliases,
                &plan.subqueries,
            ),
            CommandPlan::Delete(plan) => (
                &mut plan.view_rule_returning,
                &plan.target_qualifier,
                &plan.returning,
                &plan.returning_aliases,
                &plan.subqueries,
            ),
            _ => {
                return Err(SQLError::Internal(
                    "view rules require INSERT, UPDATE or DELETE".into(),
                ))
            }
        };
        if retained.is_some() {
            return Ok(self);
        }
        let (returning, owner) = if let Some(mut copying) = Copying::for_rewrite(*control) {
            let returning = copying.rule_returning_inputs(
                relation,
                qualifier,
                projections,
                aliases,
                queries,
            )?;
            (returning, Some(copying.into_rewrite_memory()))
        } else {
            (
                crate::plan::ViewRuleReturningPlan {
                    relation: relation.to_owned(),
                    target_qualifier: qualifier.clone(),
                    returning: projections.clone(),
                    aliases: aliases.clone(),
                    subqueries: queries.clone(),
                },
                None,
            )
        };
        *retained = Some(returning);
        if let Some(memory) = &mut self.memory {
            *body(memory).field_mut("view_rule_returning") = owner;
        }
        control.check()?;
        Ok(self)
    }
}

fn body(memory: &mut RewriteMemory) -> &mut RewriteMemory {
    memory.field_mut("value0").as_mut().expect("mutation body")
}

fn field_owner<T>(
    value: Produced<T>,
    control: &ProductionControl<'_>,
) -> Result<(T, Option<RewriteMemory>), SQLError> {
    let (value, owner) = value.into_parts();
    let (value, owner) = control.finish(value, owner)?.into_parts();
    Ok((
        value,
        owner.map(|direct| RewriteMemory::Fields {
            fields: BudgetedVec::new(direct.budget()),
            direct,
        }),
    ))
}

fn append_rule_relation(
    values: Vec<String>,
    owner: Option<crate::plan::mutable::memory::RewriteMemory>,
    relation: &str,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<(Vec<String>, Option<RewriteMemory>), SQLError> {
    let Some(mut copying) = Copying::for_rewrite(*control) else {
        let mut values = values;
        values.push(relation.to_owned());
        return Ok((values, None));
    };
    let mut previous = RelationNames {
        values: values.into_iter(),
        memory: owner.expect("rule relation vector owner"),
        index: 0,
    };
    let mut new = Some(control.copy_text(relation)?);
    let count = previous.values.len();
    let total = count
        .checked_add(1)
        .ok_or_else(|| SQLError::Internal("rule relation count overflow".into()))?;
    let items = (0..total).map(|index| {
        if index == count {
            new.take().expect("appended relation")
        } else {
            previous.take_name()
        }
    });
    let values = copying.map(items, |copying, value| Ok(copying.take(value)))?;
    drop(previous);
    Ok((values, Some(copying.into_rewrite_memory())))
}

struct RelationNames {
    values: std::vec::IntoIter<String>,
    memory: RewriteMemory,
    index: usize,
}

impl RelationNames {
    fn take_name(&mut self) -> Produced<String> {
        let value = self.values.next().expect("original relation");
        let memory = self
            .memory
            .item_mut(self.index)
            .take()
            .expect("rule relation name")
            .into_reservation();
        self.index += 1;
        Produced::from(uqa_core::memory::Budgeted::new(value, memory))
    }
}
