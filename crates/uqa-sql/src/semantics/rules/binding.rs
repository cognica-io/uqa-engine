//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Unevaluated OLD/NEW query inputs for stored rewrite rules.

mod actions;
pub use actions::{
    InsertRuleQueries, MutationRuleInput, OwnedMutationRuleInput, OwnedRuleQueryInput,
    OwnedRuleQueryResolver, RuleAction, RuleInputColumns, RuleMutation, RuleQueryInput,
    RuleQueryResolver,
};

pub struct RuleColumnMetadata {
    pub ty: crate::ast::ColumnType,
    pub collation_oid: Option<u32>,
}

impl RuleColumnMetadata {
    /// A projected OLD/NEW field is still a column of the event relation. Its source expression cannot export explicit collation strength across that boundary.
    pub fn bind_input(&self, expression: crate::ScalarExpr) -> crate::ScalarExpr {
        if !crate::schema::composites::type_is_collatable(&self.ty) {
            return expression;
        }
        let oid = self.collation_oid.unwrap_or_else(|| {
            u32::try_from(crate::catalog::type_metadata::pg_type_collation_oid(
                &self.ty,
            ))
            .expect("type collation identity")
        });
        crate::ScalarExpr::Collated {
            expr: Box::new(expression),
            ty: self.ty.clone(),
            collation: crate::ir::ResultCollation::new(
                crate::catalog::collations::derivation::Derivation::implicit(i64::from(oid)),
                oid,
            ),
        }
    }
}
