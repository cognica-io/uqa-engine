//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Analyzed rule input expressions and the physical values they reference.

use crate::{OwnedPhysicalRow, PhysicalRow, RowSchema};
use std::collections::BTreeMap;
use uqa_core::Value;
use uqa_sql::{
    ast::{ColumnType, InternalRelationId},
    plan::ExpressionPlan,
    ScalarExpr,
};

pub struct RuleInputProjection {
    pub expressions: BTreeMap<String, ExpressionPlan>,
    pub source: OwnedPhysicalRow,
}

impl Default for RuleInputProjection {
    fn default() -> Self {
        Self {
            expressions: BTreeMap::new(),
            source: OwnedPhysicalRow::new(RowSchema::default(), PhysicalRow::default()),
        }
    }
}

impl RuleInputProjection {
    pub fn values(values: impl IntoIterator<Item = (String, Value, Option<ColumnType>)>) -> Self {
        let relation = InternalRelationId::allocate();
        let mut expressions = BTreeMap::new();
        let mut fields = Vec::new();
        let mut types = Vec::new();
        for (name, value, ty) in values {
            expressions.insert(
                name,
                ExpressionPlan {
                    scalar: ScalarExpr::InternalColumn(relation.column(fields.len())),
                    subqueries: Vec::new(),
                },
            );
            fields.push(value);
            types.push(ty);
        }
        Self {
            expressions,
            source: OwnedPhysicalRow::new(
                RowSchema::with_internal_relation_types(relation, types),
                PhysicalRow::from_values(fields),
            ),
        }
    }
}

pub(super) fn append_source(target: &mut OwnedPhysicalRow, source: &OwnedPhysicalRow) {
    target.schema = RowSchema::join(&target.schema, &source.schema, std::iter::empty());
    target.row = PhysicalRow::concat(&target.row, &source.row);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scalar::plan::{eval_physical, PhysicalEvalContext};

    #[test]
    fn joined_rule_inputs_keep_runtime_types_and_lazy_errors() {
        let left = RuleInputProjection::values([(
            "guard".into(),
            Value::Bool(true),
            Some(ColumnType::Boolean),
        )]);
        let right = RuleInputProjection::values([(
            "divisor".into(),
            Value::Int(0),
            Some(ColumnType::SmallInteger),
        )]);
        let mut source = left.source;
        append_source(&mut source, &right.source);
        let uqa_sql::plan::UnifiedPlan::Query(query) = uqa_sql::plan::UnifiedPlan::lower(
            uqa_sql::compile("SELECT CASE WHEN $1 THEN 7 ELSE 1/$2 END")
                .unwrap()
                .remove(0),
        ) else {
            panic!("query");
        };
        let uqa_sql::plan::RelationalPlan::QueryBlock(mut block) = query.root else {
            panic!("query block");
        };
        let mut plan = ExpressionPlan {
            scalar: block.projections.remove(0).expr,
            subqueries: block.subqueries,
        };
        uqa_sql::plan::rewrite_scalar_expression(&mut plan.scalar, &mut |node| match node {
            ScalarExpr::Param(1) => *node = left.expressions["guard"].scalar.clone(),
            ScalarExpr::Param(2) => *node = right.expressions["divisor"].scalar.clone(),
            _ => {}
        });
        let view = source.view();
        let context =
            PhysicalEvalContext::from_row_lookup(&view, &[]).with_row_schema(&source.schema);
        assert_eq!(eval_physical(&plan, &context).unwrap(), Value::Int(7));
        assert_eq!(
            eval_physical(&right.expressions["divisor"], &context).unwrap(),
            Value::Int(0)
        );
        uqa_sql::plan::rewrite_scalar_expression(&mut plan.scalar, &mut |node| {
            if *node == left.expressions["guard"].scalar {
                *node = ScalarExpr::Literal(Value::Bool(false));
            }
        });
        assert_eq!(
            eval_physical(&plan, &context).unwrap_err().sqlstate(),
            Some("22012")
        );
        let ScalarExpr::InternalColumn(divisor) = right.expressions["divisor"].scalar else {
            panic!("runtime input");
        };
        assert_eq!(
            source.schema.internal_type(divisor),
            Some(&ColumnType::SmallInteger)
        );
    }
}
