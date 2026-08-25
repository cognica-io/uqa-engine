//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Row-trigger catalog statements.

use serde::{Deserialize, Serialize};

use super::Expr;

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
