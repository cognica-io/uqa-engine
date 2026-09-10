//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::ast::{CreateRule, CreateTrigger, EventEnableMode};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

mod rule_condition_binding;
pub use rule_condition_binding::RuleConditionBinding;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleDependencies {
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub relations: BTreeSet<uqa_core::RelationIdentity>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub columns: BTreeSet<RuleColumnDependency>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub routines: BTreeSet<RuleRoutineDependency>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RuleColumnDependency {
    pub relation: uqa_core::RelationIdentity,
    pub column: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RuleRoutineDependency {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object_id: Option<[u8; 16]>,
    pub name: String,
    pub argument_types: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredTrigger {
    pub definition: CreateTrigger,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function_object_id: Option<[u8; 16]>,
    #[serde(default)]
    pub enabled: EventEnableMode,
    #[serde(default)]
    pub object_id: Option<[u8; 16]>,
    #[serde(default)]
    pub constraint_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredRule {
    pub definition: CreateRule,
    #[serde(default)]
    pub enabled: EventEnableMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition_plan: Option<crate::plan::ExpressionPlan>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition_binding: Option<RuleConditionBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dependencies: Option<RuleDependencies>,
}

impl StoredRule {
    pub fn bound_condition_plan(
        &self,
    ) -> Option<(&crate::plan::ExpressionPlan, &RuleConditionBinding)> {
        self.condition_plan
            .as_ref()
            .zip(self.condition_binding.as_ref())
    }
}

impl StoredTrigger {
    pub fn constraint_identity(
        &self,
    ) -> Result<super::constraints::ConstraintIdentity, crate::SQLError> {
        use super::constraints::ConstraintIdentity;
        use crate::SQLError;
        use uqa_core::RelationIdentity;
        let trigger = self;
        if !trigger.definition.constraint {
            return Err(SQLError::Internal(format!(
                "ordinary trigger `{}` requested a constraint identity",
                trigger.definition.name
            )));
        }
        let relation =
            RelationIdentity::from_legacy_name(&trigger.definition.table).map_err(|error| {
                SQLError::Internal(format!(
                    "decode constraint-trigger relation `{}`: {error}",
                    trigger.definition.table
                ))
            })?;
        Ok(ConstraintIdentity {
            relation,
            name: trigger
                .constraint_name
                .clone()
                .unwrap_or_else(|| trigger.definition.name.clone()),
            object_id: trigger.object_id,
        })
    }
}

use crate::SQLError;
pub type TriggerCatalog = std::collections::BTreeMap<
    uqa_core::RelationIdentity,
    std::collections::BTreeMap<String, StoredTrigger>,
>;
pub type RuleCatalog = std::collections::BTreeMap<
    uqa_core::RelationIdentity,
    std::collections::BTreeMap<String, StoredRule>,
>;
pub mod renames;

pub fn synchronize_rule_sql_text(definition: &mut CreateRule) -> Result<(), SQLError> {
    definition.condition_sql = definition
        .condition
        .as_ref()
        .map(crate::render::expression_sql)
        .transpose()?;
    definition.action_sql = definition
        .actions
        .iter()
        .map(crate::render::statement_sql)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(())
}
