//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

pub(crate) use uqa_sql::catalog::events::RuleConditionBinding;

#[cfg(test)]
mod tests {
    use uqa_sql::ast::RuleEvent;
    use uqa_sql::ir::ScalarExpr;
    #[test]
    fn stored_rule_conditions_use_only_structural_event_row_references() {
        let engine = crate::Engine::new();
        engine
            .sql(
                "CREATE TABLE structural_rule_items(id integer);
                 CREATE RULE structural_rule_condition AS ON INSERT TO structural_rule_items
                   WHERE EXISTS (SELECT 1 WHERE NEW.id > 0) DO NOTHING",
                &[],
            )
            .unwrap();
        let rule = engine
            .rules_for("public.structural_rule_items", RuleEvent::Insert)
            .unwrap()
            .pop()
            .unwrap();
        let (plan, binding) = rule.bound_condition_plan().unwrap();
        let expected = binding.new_column("id").unwrap();
        let mut internal_columns = Vec::new();
        let mut contains_nul_qualifier = false;
        let mut inspect = |expression: &mut ScalarExpr| match expression {
            ScalarExpr::InternalColumn(column) => internal_columns.push(*column),
            ScalarExpr::QualifiedColumn { qualifier, .. } => {
                contains_nul_qualifier |= qualifier.contains('\0');
            }
            _ => {}
        };
        let mut scalar = plan.scalar.clone();
        uqa_planner::rewrite_scalar_expression(&mut scalar, &mut inspect);
        for subquery in &plan.subqueries {
            let mut subquery = subquery.clone();
            subquery.rewrite_scalar_expressions(&mut inspect);
        }
        assert!(internal_columns.contains(&expected));
        assert!(!contains_nul_qualifier);

        let serialized = serde_json::to_string(&rule).unwrap();
        assert!(!serialized.contains("\\u0000"));
        assert!(!serialized.contains('\0'));
    }
}
