//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::ast::{CreateRule, CreateTrigger, EventEnableMode};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use uqa_core::memory::OwnedSet;

mod registry;
pub(crate) use registry::EventIter;
pub use registry::{CapturedEventCatalog, EventCatalog, EventDefinitions, EventValues};

/// Dependency rewrites preserve their existing diagnostic text and keep native admission errors typed across storage publication.
#[derive(Debug, thiserror::Error)]
pub enum EventChangeError {
    #[error("{0}")]
    Definition(String),
    #[error("{0}")]
    SQL(#[from] crate::SQLError),
}

impl From<String> for EventChangeError {
    fn from(message: String) -> Self {
        Self::Definition(message)
    }
}

mod rule_condition_binding;
pub use rule_condition_binding::RuleConditionBinding;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleDependencies {
    #[serde(default, skip_serializing_if = "OwnedSet::is_empty")]
    pub relations: OwnedSet<uqa_core::RelationIdentity>,
    #[serde(default, skip_serializing_if = "OwnedSet::is_empty")]
    pub columns: OwnedSet<RuleColumnDependency>,
    #[serde(default, skip_serializing_if = "OwnedSet::is_empty")]
    pub routines: OwnedSet<RuleRoutineDependency>,
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
    /// The `pg_trigger` OID `CreateTrigger` allocated; triggers created before OIDs were recorded derive theirs from their identity and relation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_oid: Option<i64>,
    /// The `pg_constraint` OID of a constraint trigger, allocated after the trigger's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub constraint_catalog_oid: Option<i64>,
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
    /// The `pg_rewrite` OID `InsertRule` allocated; rules created before OIDs were recorded keep the OID their name derived when the catalog first opened with this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_oid: Option<i64>,
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

pub type TriggerCatalog = EventCatalog<StoredTrigger>;
pub type RuleCatalog = EventCatalog<StoredRule>;
pub mod dependencies;
pub mod renames;

/// Surviving rule definitions prepared before column metadata changes, plus the rules to rebind afterward.
pub struct PreparedRuleColumnDrop {
    pub rules: RuleCatalog,
    pub rebind: BTreeSet<(uqa_core::RelationIdentity, String)>,
}

pub mod validation;

pub mod definition;

pub mod restoration;

pub mod native;

pub mod reads;

pub mod removal;

pub mod persistence;

pub mod selection;
