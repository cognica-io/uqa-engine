//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Declared schema objects that depend on a sequence.

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum SequenceSchemaDependent {
    Default {
        table: String,
        column: String,
        foreign: bool,
    },
    GeneratedColumn {
        table: String,
        column: String,
        foreign: bool,
    },
    CheckConstraint {
        table: String,
        constraint: String,
        foreign: bool,
    },
}

impl SequenceSchemaDependent {
    pub fn table(&self) -> &str {
        match self {
            Self::Default { table, .. }
            | Self::GeneratedColumn { table, .. }
            | Self::CheckConstraint { table, .. } => table,
        }
    }

    pub fn is_column(&self, table_name: &str, column_name: &str) -> bool {
        match self {
            Self::Default { table, column, .. } | Self::GeneratedColumn { table, column, .. } => {
                table == table_name && column == column_name
            }
            Self::CheckConstraint { .. } => false,
        }
    }

    pub fn object_label(&self) -> String {
        match self {
            Self::Default {
                table,
                column,
                foreign,
            } => format!(
                "default value for column {column} of {} {table}",
                relation_kind(*foreign)
            ),
            Self::GeneratedColumn {
                table,
                column,
                foreign,
            } => format!("column {column} of {} {table}", relation_kind(*foreign)),
            Self::CheckConstraint {
                table,
                constraint,
                foreign,
            } => format!(
                "constraint {constraint} on {} {table}",
                relation_kind(*foreign)
            ),
        }
    }
}

fn relation_kind(foreign: bool) -> &'static str {
    if foreign {
        "foreign table"
    } else {
        "table"
    }
}
