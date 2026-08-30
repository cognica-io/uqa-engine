//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Row-trigger and rewrite-rule catalog statements.

use serde::{Deserialize, Serialize};

use super::{Expr, Statement};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TriggerTiming {
    Before,
    After,
    InsteadOf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TriggerEvent {
    Insert,
    Update,
    Delete,
    Truncate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriggerTransitionRelation {
    pub name: String,
    pub is_new: bool,
    pub is_table: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerDeferrability {
    #[default]
    NotDeferrable,
    InitiallyImmediate,
    InitiallyDeferred,
}

impl TriggerDeferrability {
    pub const fn is_deferrable(self) -> bool {
        !matches!(self, Self::NotDeferrable)
    }

    pub const fn is_initially_deferred(self) -> bool {
        matches!(self, Self::InitiallyDeferred)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateTrigger {
    pub name: String,
    pub table: String,
    pub function: String,
    pub arguments: Vec<String>,
    #[serde(default)]
    pub constraint: bool,
    #[serde(default)]
    pub referenced_table: Option<String>,
    #[serde(default)]
    pub deferrability: TriggerDeferrability,
    pub row: bool,
    pub timing: TriggerTiming,
    pub events: Vec<TriggerEvent>,
    pub update_columns: Vec<String>,
    #[serde(default)]
    pub transition_relations: Vec<TriggerTransitionRelation>,
    pub when: Option<Expr>,
    pub or_replace: bool,
}

impl CreateTrigger {
    #[must_use]
    pub fn old_transition_table(&self) -> Option<&str> {
        self.transition_relations
            .iter()
            .find(|relation| relation.is_table && !relation.is_new)
            .map(|relation| relation.name.as_str())
    }

    #[must_use]
    pub fn new_transition_table(&self) -> Option<&str> {
        self.transition_relations
            .iter()
            .find(|relation| relation.is_table && relation.is_new)
            .map(|relation| relation.name.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DropTrigger {
    pub name: String,
    pub table: String,
    pub if_exists: bool,
    pub cascade: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuleEvent {
    Select,
    Insert,
    Update,
    Delete,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateRule {
    pub name: String,
    pub table: String,
    pub event: RuleEvent,
    pub instead: bool,
    pub condition: Option<Expr>,
    pub actions: Vec<Statement>,
    #[serde(default)]
    pub action_sql: Vec<String>,
    pub or_replace: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DropRule {
    pub name: String,
    pub table: String,
    pub if_exists: bool,
    pub cascade: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventEnableMode {
    #[default]
    Origin,
    Disabled,
    Replica,
    Always,
}

impl EventEnableMode {
    #[must_use]
    pub const fn catalog_code(self) -> &'static str {
        match self {
            Self::Origin => "O",
            Self::Disabled => "D",
            Self::Replica => "R",
            Self::Always => "A",
        }
    }

    #[must_use]
    pub const fn fires_in_origin(self) -> bool {
        matches!(self, Self::Origin | Self::Always)
    }

    #[must_use]
    pub const fn fires_in_replica(self) -> bool {
        matches!(self, Self::Replica | Self::Always)
    }
}
