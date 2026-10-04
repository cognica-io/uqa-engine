//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Why a view cannot be rewritten onto its base relation, and the errors that report it as `RewriteQuery`, `rewriteTargetView` and `error_view_not_updatable` do.

use super::{display_relation, SQLError};

/// Why a view cannot be rewritten onto its base relation. `RewriteQuery` tests conditional `INSTEAD` rules first, and `view_query_is_auto_updatable` tests the rest in the order listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotUpdatableReason {
    ConditionalInsteadRule,
    Distinct,
    GroupBy,
    Having,
    SetOperation,
    With,
    LimitOffset,
    Aggregate,
    Window,
    SetReturning,
    NotSingleRelation,
    /// INSERT and UPDATE need an updatable column; DELETE does not.
    NoUpdatableColumns,
}

impl NotUpdatableReason {
    /// The DETAIL that reports the reason.
    pub const fn detail(self) -> &'static str {
        match self {
            Self::ConditionalInsteadRule => {
                "Views with conditional DO INSTEAD rules are not automatically updatable."
            }
            Self::Distinct => "Views containing DISTINCT are not automatically updatable.",
            Self::GroupBy => "Views containing GROUP BY are not automatically updatable.",
            Self::Having => "Views containing HAVING are not automatically updatable.",
            Self::SetOperation => {
                "Views containing UNION, INTERSECT, or EXCEPT are not automatically updatable."
            }
            Self::With => "Views containing WITH are not automatically updatable.",
            Self::LimitOffset => "Views containing LIMIT or OFFSET are not automatically updatable.",
            Self::Aggregate => {
                "Views that return aggregate functions are not automatically updatable."
            }
            Self::Window => "Views that return window functions are not automatically updatable.",
            Self::SetReturning => {
                "Views that return set-returning functions are not automatically updatable."
            }
            Self::NotSingleRelation => {
                "Views that do not select from a single table or view are not automatically updatable."
            }
            Self::NoUpdatableColumns => {
                "Views that have no updatable columns are not automatically updatable."
            }
        }
    }
}

/// Why a view column cannot be written, as `view_col_is_auto_updatable` reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnRestriction {
    /// The column is an expression, not a column of the base relation.
    Computed,
    /// The column refers to a system column of the base relation.
    SystemColumn,
}

impl ColumnRestriction {
    const fn detail(self) -> &'static str {
        match self {
            Self::Computed => {
                "View columns that are not columns of their base relation are not updatable."
            }
            Self::SystemColumn => "View columns that refer to system columns are not updatable.",
        }
    }
}

/// The command a view must perform, by itself or as an action of a MERGE.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewCommand {
    Insert,
    Update,
    Delete,
    MergeInsert,
    MergeUpdate,
    MergeDelete,
}

/// `error_view_not_updatable`: `55000` naming the command, the reason as DETAIL, and as HINT what would let the command through the view; MERGE supports no rules, so its hint names only a trigger.
pub fn view_not_updatable(
    view: &str,
    command: ViewCommand,
    reason: NotUpdatableReason,
) -> SQLError {
    let view = display_relation(view);
    let (message, hint) = match command {
        ViewCommand::Insert => (
            format!("cannot insert into view \"{view}\""),
            "To enable inserting into the view, provide an INSTEAD OF INSERT trigger or an unconditional ON INSERT DO INSTEAD rule.",
        ),
        ViewCommand::Update => (
            format!("cannot update view \"{view}\""),
            "To enable updating the view, provide an INSTEAD OF UPDATE trigger or an unconditional ON UPDATE DO INSTEAD rule.",
        ),
        ViewCommand::Delete => (
            format!("cannot delete from view \"{view}\""),
            "To enable deleting from the view, provide an INSTEAD OF DELETE trigger or an unconditional ON DELETE DO INSTEAD rule.",
        ),
        ViewCommand::MergeInsert => (
            format!("cannot insert into view \"{view}\""),
            "To enable inserting into the view using MERGE, provide an INSTEAD OF INSERT trigger.",
        ),
        ViewCommand::MergeUpdate => (
            format!("cannot update view \"{view}\""),
            "To enable updating the view using MERGE, provide an INSTEAD OF UPDATE trigger.",
        ),
        ViewCommand::MergeDelete => (
            format!("cannot delete from view \"{view}\""),
            "To enable deleting from the view using MERGE, provide an INSTEAD OF DELETE trigger.",
        ),
    };
    SQLError::Diagnostic {
        sqlstate: "55000".into(),
        message,
        detail: Some(reason.detail().into()),
        hint: Some(hint.into()),
    }
}

/// The statement whose write of a view column a view rejects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnWrite {
    Insert,
    Update,
    Merge,
}

/// `rewriteTargetView`'s `0A000` for a statement that writes a column the view cannot write, with the reason as DETAIL.
pub fn non_writable_column(
    view: &str,
    column: &str,
    write: ColumnWrite,
    restriction: ColumnRestriction,
) -> SQLError {
    let verb = match write {
        ColumnWrite::Insert => "insert into",
        ColumnWrite::Update => "update",
        ColumnWrite::Merge => "merge into",
    };
    SQLError::Diagnostic {
        sqlstate: "0A000".into(),
        message: format!(
            "cannot {verb} column \"{column}\" of view \"{}\"",
            display_relation(view)
        ),
        detail: Some(restriction.detail().into()),
        hint: None,
    }
}

/// `rewriteTargetView`'s `0A000` for a MERGE some of whose actions have an INSTEAD OF trigger on an automatically updatable view while others do not.
pub fn mixed_merge_paths(view: &str) -> SQLError {
    SQLError::Diagnostic {
        sqlstate: "0A000".into(),
        message: format!("cannot merge into view \"{}\"", display_relation(view)),
        detail: Some(
            "MERGE is not supported for views with INSTEAD OF triggers for some actions but not all."
                .into(),
        ),
        hint: Some(
            "To enable merging into the view, either provide a full set of INSTEAD OF triggers or drop the existing INSTEAD OF triggers."
                .into(),
        ),
    }
}

/// The `0A000` of a MERGE whose target, or a view it is rewritten through, has rules, which `transformMergeStmt` and `RewriteQuery` reject.
pub fn merge_with_rules(relation: &str) -> SQLError {
    SQLError::Diagnostic {
        sqlstate: "0A000".into(),
        message: format!(
            "cannot execute MERGE on relation \"{}\"",
            display_relation(relation)
        ),
        detail: Some("MERGE is not supported for relations with rules.".into()),
        hint: None,
    }
}

/// `transformMergeStmt`'s `0A000` for a MERGE into a materialized view, with `errdetail_relkind_not_supported`'s DETAIL.
pub fn merge_into_materialized_view(relation: &str) -> SQLError {
    SQLError::Diagnostic {
        sqlstate: "0A000".into(),
        message: format!(
            "cannot execute MERGE on relation \"{}\"",
            display_relation(relation)
        ),
        detail: Some("This operation is not supported for materialized views.".into()),
        hint: None,
    }
}
