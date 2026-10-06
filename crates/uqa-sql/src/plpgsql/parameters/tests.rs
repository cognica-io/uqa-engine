//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//
use super::*;
use crate::{binding::VariableSiteResolution, ColumnType};
use uqa_core::Value;
struct Variables(i64);
impl VariableResolver for Variables {
    fn resolve_name(&mut self, name: &str) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok((name == "v").then(|| ResolvedVariable {
            value: Value::Int(self.0),
            declared_type: Some("integer".into()),
        }))
    }
    fn resolve_qualified(
        &mut self,
        _: &str,
        _: &str,
    ) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }
    fn resolve_param(&mut self, index: usize) -> Result<Option<ResolvedVariable>, SQLError> {
        if index == 1 {
            self.resolve_name("v")
        } else {
            Ok(None)
        }
    }
}
#[test]
fn prepared_variable_sites_keep_types_and_read_fresh_values() {
    let statement = crate::compile("SELECT v + $1").unwrap().remove(0);
    let bound = parameterize_statement_variables(
        &statement,
        &mut Variables(4),
        VariableConflict::Error,
        &mut |_, _, sites| Ok(vec![VariableSiteResolution::Variable; sites.len()]),
    )
    .unwrap();
    let Statement::Select(query) = &bound.statement else {
        panic!("SELECT")
    };
    let Expr::Binary {
        lhs: left,
        rhs: right,
        ..
    } = &query.projections[0].expr
    else {
        panic!("addition")
    };
    assert!(matches!(&**left, Expr::Param(1)));
    assert!(matches!(&**right, Expr::Param(2)));
    assert_eq!(bound.parameters.len(), 2);
    for reference in &bound.references {
        let parameter = reference.read(&mut Variables(9)).unwrap();
        assert_eq!(parameter.scalar_value(), Some(&Value::Int(9)));
        assert_eq!(parameter.declared_scalar_type(), Some(&ColumnType::Integer));
    }
}
#[test]
fn selected_columns_are_not_captured_as_routine_arguments() {
    let statement = crate::compile("SELECT v").unwrap().remove(0);
    let bound = parameterize_statement_variables(
        &statement,
        &mut Variables(4),
        VariableConflict::UseColumn,
        &mut |_, _, _| Ok(vec![VariableSiteResolution::Column]),
    )
    .unwrap();
    assert!(bound.references.is_empty());
    let Statement::Select(query) = bound.statement else {
        panic!("SELECT")
    };
    assert!(matches!(&query.projections[0].expr,Expr::Column(v) if v=="v"));
}
