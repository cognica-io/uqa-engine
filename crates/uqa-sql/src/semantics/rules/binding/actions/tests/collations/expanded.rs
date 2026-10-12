//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::semantics::rules::action_binding::{
    bind_rule_action, expand_rule_action_row_stars, RuleSourceCatalog,
};

struct Columns(Vec<crate::ast::ColumnDef>);
impl RuleSourceCatalog for Columns {
    fn query_source_columns(&self, _: &str, _: bool) -> Result<Option<Vec<String>>, SQLError> {
        Ok(Some(
            self.0.iter().map(|column| column.name.clone()).collect(),
        ))
    }
    fn rule_relation_definitions(&self, _: &str) -> Result<Vec<crate::ast::ColumnDef>, SQLError> {
        Ok(self.0.clone())
    }
}

#[test]
fn expanded_rule_fields_keep_postgresql_event_collations() {
    let (columns, rows) = inputs();
    let mut ordered: Vec<_> = columns.iter().collect();
    ordered.sort_by_key(|(_, column)| column.position);
    let catalog = Columns(
        ordered
            .iter()
            .map(|(name, column)| {
                let mut definition =
                    crate::ast::ColumnDef::nullable((*name).clone(), column.ty.clone());
                definition.collation_oid = column.collation_oid;
                definition
            })
            .collect(),
    );
    let row_type: Vec<_> = catalog
        .0
        .iter()
        .map(|column| (column.name.clone(), column.ty.clone()))
        .collect();
    let action_columns = catalog.0.iter().map(|column| column.name.clone()).collect();
    let mut cases: Vec<Reference> = serde_json::from_str(include_str!("expanded.json")).unwrap();
    assert_eq!(
        cases.iter().map(|case| case.variables.len()).sum::<usize>(),
        64
    );
    let shadowed: Vec<Reference> = serde_json::from_str(include_str!("shadowed.json")).unwrap();
    assert_eq!(
        shadowed
            .iter()
            .map(|case| case.variables.len())
            .sum::<usize>(),
        16
    );
    cases.extend(shadowed);
    for case in cases {
        let Statement::CreateRule(rule) = crate::compile(&case.sql).unwrap().remove(0) else {
            panic!("rule")
        };
        let expanded = expand_rule_action_row_stars(
            &catalog,
            &rule.actions[0],
            &action_columns,
            &row_type,
            rule.event,
        )
        .unwrap();
        for matching in [&[0][..], &[][..]] {
            let bound = bind_set_oriented_action(matching, &rows, &columns, &|resolver| {
                bind_rule_action(&catalog, &expanded, &action_columns, resolver)
            })
            .unwrap();
            let Statement::Insert(insert) = bound.statement else {
                panic!("insert")
            };
            check_schema(&case, insert.select_source.unwrap());
        }
    }
}
