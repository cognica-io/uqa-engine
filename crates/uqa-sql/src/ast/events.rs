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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TriggerEvent {
    Insert,
    Update,
    Delete,
    Truncate,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateTrigger {
    pub name: String,
    pub table: String,
    pub function: String,
    pub arguments: Vec<String>,
    pub row: bool,
    pub timing: TriggerTiming,
    pub events: Vec<TriggerEvent>,
    pub update_columns: Vec<String>,
    pub when: Option<Expr>,
    pub or_replace: bool,
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
}
