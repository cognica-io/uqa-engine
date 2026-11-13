//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, Input, InputError, JsonToken, Native, Parts, Stored, Values};
use crate::ast::events::{
    CreateRule, CreateTrigger, DropRule, DropTrigger, EventEnableMode, RuleEvent,
    TriggerDeferrability, TriggerEvent, TriggerTiming, TriggerTransitionRelation,
};

stored_struct!(CreateRule {
    name: required,
    table: required,
    event: required,
    instead: required,
    condition: optional,
    actions: required,
    or_replace: required,
});

stored_struct!(CreateTrigger {
    name: required,
    table: required,
    function: required,
    arguments: required,
    constraint: defaulted,
    referenced_table: defaulted,
    deferrability: defaulted,
    row: required,
    timing: required,
    events: required,
    update_columns: required,
    transition_relations: defaulted,
    when: optional,
    or_replace: required,
});

stored_struct!(DropRule {
    name: required,
    table: required,
    if_exists: required,
    cascade: required,
});

stored_struct!(DropTrigger {
    name: required,
    table: required,
    if_exists: required,
    cascade: required,
});

stored_enum!(EventEnableMode {
    Origin,
    Disabled,
    Replica,
    Always,
});

stored_enum!(RuleEvent {
    Select,
    Insert,
    Update,
    Delete,
});

units!(TriggerDeferrability);

stored_enum!(TriggerEvent {
    Insert,
    Update,
    Delete,
    Truncate,
});

stored_enum!(TriggerTiming {
    Before,
    After,
    InsteadOf,
});

stored_struct!(TriggerTransitionRelation {
    name: required,
    is_new: required,
    is_table: required,
});
