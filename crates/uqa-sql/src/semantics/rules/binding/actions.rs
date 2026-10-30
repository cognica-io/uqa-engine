//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native rule actions share one query-input binding path.

mod query;
pub use query::{
    InsertRuleQueries, MutationRuleInput, RuleAction, RuleMutation, RuleQueryInput,
    RuleQueryResolver,
};

#[cfg(test)]
mod tests;
