//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Private plan metadata reuses the existing field, vector, type and function-binding retirement owners.

use super::{fields, scalar_fields, Retire, Retirement};
use crate::{
    ast::{
        ColumnType, FunctionBinding, JoinUsing, LockingClause, OperatorJoinRelations,
        ReturningAliases,
    },
    plan::{CtePlan, CteSearchPlan, ViewRuleInsertPlan, ViewRuleReturningPlan, ViewRuleUpdatePlan},
};

impl Retire for String {
    fn retire_step(&mut self, _: &mut [usize], _: &mut Option<Retirement>) -> bool {
        if self.capacity() == 0 {
            true
        } else {
            drop(std::mem::take(self));
            false
        }
    }
}

impl Retire for ColumnType {
    fn retire_step(&mut self, _: &mut [usize], _: &mut Option<Retirement>) -> bool {
        ColumnType::retire_step(self)
    }
}

impl Retire for FunctionBinding {
    fn retire_step(&mut self, _: &mut [usize], _: &mut Option<Retirement>) -> bool {
        FunctionBinding::retire_step(self)
    }
}

scalar_fields!(CtePlan { cycle, name, columns, search } skip { recursive, materialization, body });
scalar_fields!(CteSearchPlan { columns, sequence_column } skip { breadth_first });
scalar_fields!(JoinUsing { columns, alias });
scalar_fields!(OperatorJoinRelations { left, right });
scalar_fields!(ReturningAliases { old, new } skip { old_explicit, new_explicit });
scalar_fields!(LockingClause { relations } skip { strength, wait });
scalar_fields!(ViewRuleReturningPlan { returning, relation, target_qualifier, aliases } skip { subqueries });
scalar_fields!(ViewRuleInsertPlan {
    relation,
    supplied_columns,
    input_columns
});
scalar_fields!(ViewRuleUpdatePlan {
    relation,
    assigned_columns,
    input_columns
});
