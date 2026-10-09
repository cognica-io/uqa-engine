//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_sql::{
    ast::{ColumnDef, EventEnableMode, RelationPersistence, TableConstraintSet},
    catalog::{
        roles::RoleDefinition, security::BoundTableSecurity, stored_view::StoredViewDefinition,
        view::StoredViewKind,
    },
    ColumnType, Statement,
};

pub(super) fn fixture(unrelated: usize) -> CatalogReadView {
    let mut snapshot = empty_catalog().snapshot().clone();
    snapshot.definitions.schemas =
        Arc::new(crate::catalog::security::BoundSchemaSecurity::initial_catalog());
    snapshot.definitions.roles = Arc::new(BTreeMap::from([(
        "uqa".into(),
        RoleDefinition::bootstrap(),
    )]));
    let relation = RelationIdentity::new("public", "items");
    snapshot.tables.insert(
        relation.clone(),
        table_snapshot(
            [1; 16],
            vec![ColumnDef::nullable("id", ColumnType::Integer)],
            TableConstraintSet::default(),
        ),
    );
    for position in 0..=unrelated {
        let Statement::CreateTrigger(definition) = uqa_sql::compile(&format!("CREATE TRIGGER watch_{position:03} BEFORE INSERT ON public.items FOR EACH ROW EXECUTE FUNCTION handler()" )).unwrap().remove(0) else { panic!("trigger"); };
        let trigger = StoredTrigger {
            definition,
            function_object_id: None,
            enabled: EventEnableMode::Origin,
            object_id: Some((position as u128 + 1).to_le_bytes()),
            constraint_name: None,
            catalog_oid: Some(70_000 + i64::try_from(position).unwrap()),
            constraint_catalog_oid: None,
        };
        Arc::make_mut(&mut snapshot.definitions.triggers)
            .entry(relation.clone())
            .or_default()
            .insert(trigger.definition.name.clone(), trigger);
        let Statement::CreateRule(definition) = uqa_sql::compile(&format!(
            "CREATE RULE rule_{position:03} AS ON UPDATE TO public.items DO ALSO NOTHING"
        ))
        .unwrap()
        .remove(0) else {
            panic!("rule");
        };
        let rule = StoredRule {
            definition,
            enabled: EventEnableMode::Origin,
            condition_plan: None,
            condition_binding: None,
            dependencies: None,
            catalog_oid: Some(80_000 + i64::try_from(position).unwrap()),
        };
        Arc::make_mut(&mut snapshot.definitions.rules)
            .entry(relation.clone())
            .or_default()
            .insert(rule.definition.name.clone(), rule);
        Arc::make_mut(&mut snapshot.definitions.views).insert(
            RelationIdentity::new("public", format!("view_{position:03}")),
            view(position),
        );
    }
    CatalogReadView::new(snapshot)
}

fn view(position: usize) -> StoredView {
    let uqa_sql::plan::UnifiedPlan::Query(query) =
        uqa_sql::plan::UnifiedPlan::lower(uqa_sql::compile("SELECT 1 AS value").unwrap().remove(0))
    else {
        panic!("query");
    };
    let oid = 90_000 + u32::try_from(position).unwrap() * 10;
    StoredView {
        security: BoundTableSecurity::owner(uqa_core::catalog_role::RoleIdentity::BOOTSTRAP),
        definition: StoredViewDefinition {
            object_id: (position as u128 + 1).to_le_bytes(),
            query: *query,
            output_columns: Some(vec!["value".into()]),
            persistence: RelationPersistence::Permanent,
            options: Vec::new(),
            kind: if position.is_multiple_of(2) {
                StoredViewKind::View
            } else {
                StoredViewKind::Materialized
            },
            materialized_rows: Vec::new(),
            materialized_column_types: Vec::new(),
            populated: true,
            catalog_oids: Some(uqa_sql::catalog::relation_oids::RelationCatalogOids {
                relation: oid,
                row_type: Some(oid + 1),
                array_type: Some(oid + 2),
                rule: Some(oid + 3),
            }),
            row_type_array_name: None,
        },
    }
}
