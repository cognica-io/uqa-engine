//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Copying, Result, SyntaxCopy};

syntax_struct!(crate::ast::CreateRule {
    name,
    table,
    event,
    instead,
    condition,
    actions,
    or_replace
});

syntax_struct!(crate::ast::CreateTrigger {
    name,
    table,
    function,
    arguments,
    constraint,
    referenced_table,
    deferrability,
    row,
    timing,
    events,
    update_columns,
    transition_relations,
    when,
    or_replace
});

syntax_struct!(crate::ast::DropRule {
    name,
    table,
    if_exists,
    cascade
});

syntax_struct!(crate::ast::DropTrigger {
    name,
    table,
    if_exists,
    cascade
});

syntax_enum!(crate::ast::EventEnableMode {
    Origin,
    Disabled,
    Replica,
    Always
});

syntax_enum!(crate::ast::RuleEvent {
    Select,
    Insert,
    Update,
    Delete
});

syntax_enum!(crate::ast::TriggerDeferrability {
    NotDeferrable,
    InitiallyImmediate,
    InitiallyDeferred
});

syntax_enum!(crate::ast::TriggerEvent {
    Insert,
    Update,
    Delete,
    Truncate
});

syntax_enum!(crate::ast::TriggerTiming {
    Before,
    After,
    InsteadOf
});

syntax_struct!(crate::ast::TriggerTransitionRelation {
    name,
    is_new,
    is_table
});
