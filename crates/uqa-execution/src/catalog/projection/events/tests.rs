//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::test_support::{empty_catalog, table_snapshot, CatalogServices};
use std::sync::Arc;
use uqa_sql::{
    ast::{EventEnableMode, TableConstraintSet},
    catalog::{roles::RoleDefinition, security::BoundSchemaSecurity},
    Statement,
};

#[test]
fn stored_constraints_and_triggers_ignore_invoking_role_schema_usage() {
    let mut snapshot = empty_catalog().snapshot().clone();
    let mut reader = RoleDefinition::bootstrap();
    reader.name = "reader".into();
    reader.oid = 20_000;
    reader.object_id = [8; 16];
    reader.attributes.clear();
    snapshot.definitions.roles = Arc::new(BTreeMap::from([
        ("uqa".into(), RoleDefinition::bootstrap()),
        (reader.name.clone(), reader),
    ]));
    snapshot.definitions.schemas = Arc::new(BTreeMap::from([(
        "hidden".into(),
        BoundSchemaSecurity::owner(uqa_sql::catalog::roles::RoleIdentity::BOOTSTRAP),
    )]));
    let Statement::CreateTable(mut table) = uqa_sql::compile(
        "CREATE TABLE hidden.items(id integer, CONSTRAINT positive CHECK(id > 0))",
    )
    .unwrap()
    .remove(0) else {
        panic!("table");
    };
    table.checks[0].object_id = Some([2; 16]);
    table.checks[0].catalog_oid = Some(70_001);
    let relation = RelationIdentity::new("hidden", "items");
    snapshot.tables.insert(
        relation.clone(),
        table_snapshot(
            [1; 16],
            table.columns,
            TableConstraintSet {
                checks: table.checks,
                ..TableConstraintSet::default()
            },
        ),
    );
    let Statement::CreateTrigger(definition) = uqa_sql::compile(
        "CREATE TRIGGER watch BEFORE INSERT ON hidden.items FOR EACH ROW EXECUTE FUNCTION handler()",
    )
    .unwrap()
    .remove(0) else {
        panic!("trigger");
    };
    snapshot.definitions.triggers = Arc::new(BTreeMap::from([(
        relation,
        BTreeMap::from([(
            "watch".into(),
            StoredTrigger {
                definition,
                function_object_id: None,
                enabled: EventEnableMode::Origin,
                object_id: Some([3; 16]),
                constraint_name: None,
                catalog_oid: Some(70_002),
                constraint_catalog_oid: None,
            },
        )]),
    )]));
    let catalog = CatalogReadView::new(snapshot);
    let mut resolution = CatalogServices::default().resolution;
    resolution.current_user = "reader".into();
    resolution.set_lookup_mode(crate::catalog::RelationLookupMode::Dynamic);
    let Err(error) = catalog.table(&resolution, "hidden.items") else {
        panic!("dynamic lookup must require schema USAGE");
    };
    assert_eq!(error.sqlstate(), Some("42501"));
    let constraints =
        super::super::helpers::constraints::constraint_catalog_rows(&catalog, &resolution).unwrap();
    assert_eq!(constraints.len(), 1);
    assert_eq!(constraints[0].catalog_oid, Some(70_001));
    let triggers = catalog_triggers(&catalog, &resolution).unwrap();
    assert_eq!(triggers.len(), 1);
    assert_eq!(triggers[0].0.catalog_oid, Some(70_002));
}
