//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native rule actions share one query-input binding path.

mod query;
pub use query::{
    MutationRuleInput, OwnedInsertRuleQueries, OwnedMutationRuleInput, OwnedRuleQueryInput,
    OwnedRuleQueryResolver, RuleAction, RuleInputColumns, RuleMutation, RuleQueryInput,
    RuleQueryResolver,
};

#[cfg(test)]
mod tests;

#[cfg(test)]
use query::InsertRuleQueries;
