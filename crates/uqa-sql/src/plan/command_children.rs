//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Direct relational children of data-modifying commands.

use super::{
    CommandPlan, CtePlan, DeletePlan, InsertPlan, MergePlan, ProjectionPlan, QueryPlan, SourcePlan,
    UpdatePlan,
};

impl CommandPlan {
    /// WITH definitions owned by this command, in declaration order.
    pub fn ctes(&self) -> &[CtePlan] {
        match self {
            Self::Insert(plan) => &plan.ctes,
            Self::Update(plan) => &plan.ctes,
            Self::Delete(plan) => &plan.ctes,
            Self::Merge(plan) => &plan.ctes,
            _ => &[],
        }
    }

    pub fn ctes_mut(&mut self) -> Option<&mut Vec<CtePlan>> {
        match self {
            Self::Insert(plan) => Some(&mut plan.ctes),
            Self::Update(plan) => Some(&mut plan.ctes),
            Self::Delete(plan) => Some(&mut plan.ctes),
            Self::Merge(plan) => Some(&mut plan.ctes),
            _ => None,
        }
    }

    /// Borrow query children evaluated in the command's WITH scope without collecting a child list. INSERT's source precedes the scalar-query arena. Source-plan subqueries are owned by `source_input`; visitors should not traverse either owner twice.
    pub fn query_inputs(&self) -> impl Iterator<Item = &QueryPlan> {
        let (source, subqueries): (Option<&QueryPlan>, &[QueryPlan]) = match self {
            Self::Insert(plan) => (plan.source.as_deref(), &plan.subqueries),
            Self::Update(plan) => (None, &plan.subqueries),
            Self::Delete(plan) => (None, &plan.subqueries),
            Self::Merge(plan) => (None, &plan.subqueries),
            _ => (None, &[]),
        };
        source.into_iter().chain(subqueries)
    }

    /// Borrow mutable query children in the same source-then-arena order, without allocating a reference vector.
    pub fn query_inputs_mut(&mut self) -> impl Iterator<Item = &mut QueryPlan> {
        let (source, subqueries): (Option<&mut QueryPlan>, &mut [QueryPlan]) = match self {
            Self::Insert(plan) => (plan.source.as_deref_mut(), &mut plan.subqueries),
            Self::Update(plan) => (None, &mut plan.subqueries),
            Self::Delete(plan) => (None, &mut plan.subqueries),
            Self::Merge(plan) => (None, &mut plan.subqueries),
            _ => (None, &mut []),
        };
        source.into_iter().chain(subqueries)
    }

    pub fn source_input(&self) -> Option<&SourcePlan> {
        match self {
            Self::Update(plan) => plan.source_input(),
            Self::Delete(plan) => plan.source_input(),
            Self::Merge(plan) => plan.source_input(),
            _ => None,
        }
    }

    pub fn source_input_mut(&mut self) -> Option<&mut SourcePlan> {
        match self {
            Self::Update(plan) => plan.source.as_deref_mut(),
            Self::Delete(plan) => plan.source.as_deref_mut(),
            Self::Merge(plan) => Some(&mut plan.source),
            _ => None,
        }
    }

    pub fn mutation_target(&self) -> Option<&str> {
        match self {
            Self::Insert(plan) => Some(&plan.table),
            Self::Update(plan) => Some(&plan.table),
            Self::Delete(plan) => Some(&plan.table),
            Self::Merge(plan) => Some(&plan.target),
            _ => None,
        }
    }

    pub fn mutation_target_mut(&mut self) -> Option<&mut String> {
        match self {
            Self::Insert(plan) => Some(&mut plan.table),
            Self::Update(plan) => Some(&mut plan.table),
            Self::Delete(plan) => Some(&mut plan.table),
            Self::Merge(plan) => Some(&mut plan.target),
            _ => None,
        }
    }

    pub fn returning(&self) -> Option<&[ProjectionPlan]> {
        match self {
            Self::Insert(plan) => Some(&plan.returning),
            Self::Update(plan) => Some(&plan.returning),
            Self::Delete(plan) => Some(&plan.returning),
            Self::Merge(plan) => Some(&plan.returning),
            _ => None,
        }
    }

    pub fn returning_mut(&mut self) -> Option<&mut [ProjectionPlan]> {
        match self {
            Self::Insert(plan) => Some(&mut plan.returning),
            Self::Update(plan) => Some(&mut plan.returning),
            Self::Delete(plan) => Some(&mut plan.returning),
            Self::Merge(plan) => Some(&mut plan.returning),
            _ => None,
        }
    }
}

impl CommandPlan {
    pub fn scalar_subqueries(&self) -> &[QueryPlan] {
        match self {
            Self::Insert(plan) => &plan.subqueries,
            Self::Update(plan) => &plan.subqueries,
            Self::Delete(plan) => &plan.subqueries,
            Self::Merge(plan) => &plan.subqueries,
            _ => &[],
        }
    }

    pub fn target_qualifier(&self) -> Option<&str> {
        match self {
            Self::Insert(plan) => Some(&plan.target_qualifier),
            Self::Update(plan) => Some(&plan.target_qualifier),
            Self::Delete(plan) => Some(&plan.target_qualifier),
            Self::Merge(plan) => Some(&plan.target_qualifier),
            _ => None,
        }
    }

    pub fn returning_aliases(&self) -> Option<&crate::ast::ReturningAliases> {
        match self {
            Self::Insert(plan) => Some(&plan.returning_aliases),
            Self::Update(plan) => Some(&plan.returning_aliases),
            Self::Delete(plan) => Some(&plan.returning_aliases),
            Self::Merge(plan) => Some(&plan.returning_aliases),
            _ => None,
        }
    }
}

impl InsertPlan {
    /// Query children evaluated in the command's WITH scope: the source query and every `subqueries` entry.
    pub fn query_inputs(&self) -> impl Iterator<Item = &QueryPlan> {
        self.source
            .iter()
            .map(Box::as_ref)
            .chain(self.subqueries.iter())
    }
}

impl UpdatePlan {
    /// Query children evaluated in the command's WITH scope, which are its `subqueries` entries.
    pub fn query_inputs(&self) -> impl Iterator<Item = &QueryPlan> {
        self.subqueries.iter()
    }

    /// The `FROM` relation the command joins to its target.
    pub fn source_input(&self) -> Option<&SourcePlan> {
        self.source.as_deref()
    }
}

impl DeletePlan {
    /// Query children evaluated in the command's WITH scope, which are its `subqueries` entries.
    pub fn query_inputs(&self) -> impl Iterator<Item = &QueryPlan> {
        self.subqueries.iter()
    }

    /// The `USING` relation the command joins to its target.
    pub fn source_input(&self) -> Option<&SourcePlan> {
        self.source.as_deref()
    }
}

impl MergePlan {
    /// Query children evaluated in the command's WITH scope, which are its `subqueries` entries.
    pub fn query_inputs(&self) -> impl Iterator<Item = &QueryPlan> {
        self.subqueries.iter()
    }

    /// The `USING` relation the command merges into its target.
    pub fn source_input(&self) -> Option<&SourcePlan> {
        Some(&self.source)
    }
}
