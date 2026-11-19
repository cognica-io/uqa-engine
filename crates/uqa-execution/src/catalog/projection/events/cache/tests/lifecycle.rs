//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn retained_event_addresses_survive_rename_and_removal() {
    let catalog = fixture(0);
    let resolution = CatalogServices::default().resolution;
    let original_trigger = trigger_by_oid(&catalog, &resolution, 70_000)
        .unwrap()
        .unwrap();
    let original_rule = rule_by_oid(&catalog, 80_000).unwrap();
    let original_view = view_rule_by_oid(&catalog, 90_003).unwrap().1;
    let mut snapshot = catalog.snapshot().clone();
    let relation = RelationIdentity::new("public", "items");
    uqa_core::memory::RetainedValue::make_mut(&mut snapshot.definitions.triggers)
        .edit(&relation, |triggers| {
            let mut trigger = triggers["watch_000"].clone();
            triggers.remove("watch_000")?;
            trigger.definition.name = "renamed_trigger".into();
            triggers.insert(trigger.definition.name.clone(), trigger)
        })
        .unwrap()
        .unwrap();
    uqa_core::memory::RetainedValue::make_mut(&mut snapshot.definitions.rules)
        .edit(&relation, |rules| {
            let mut rule = rules["rule_000"].clone();
            rules.remove("rule_000")?;
            rule.definition.name = "renamed_rule".into();
            rules.insert(rule.definition.name.clone(), rule)
        })
        .unwrap()
        .unwrap();
    let views = uqa_core::memory::RetainedValue::make_mut(&mut snapshot.definitions.views);
    let relation = RelationIdentity::new("public", "view_000");
    let view = views[&relation].clone();
    assert!(views.remove(&relation).unwrap());
    views
        .insert(RelationIdentity::new("public", "renamed_view"), view)
        .unwrap();
    let current = CatalogReadView::new(snapshot.clone());
    assert_eq!(
        trigger_by_oid(&current, &resolution, 70_000)
            .unwrap()
            .unwrap()
            .definition
            .name,
        "renamed_trigger"
    );
    assert_eq!(
        rule_by_oid(&current, 80_000).unwrap().definition.name,
        "renamed_rule"
    );
    assert_eq!(
        view_rule_by_oid(&current, 90_003).unwrap().0.name,
        "renamed_view"
    );
    snapshot.definitions.triggers = uqa_core::memory::RetainedValue::default();
    snapshot.definitions.rules = uqa_core::memory::RetainedValue::default();
    snapshot.definitions.views = uqa_core::memory::RetainedValue::default();
    let removed = CatalogReadView::new(snapshot);
    assert!(trigger_by_oid(&removed, &resolution, 70_000)
        .unwrap()
        .is_none());
    assert!(rule_by_oid(&removed, 80_000).is_none());
    assert!(view_rule_by_oid(&removed, 90_003).is_none());
    assert_eq!(original_trigger.definition.name, "watch_000");
    assert_eq!(original_rule.definition.name, "rule_000");
    assert!(std::ptr::eq(
        original_view,
        view_rule_by_oid(&catalog, 90_003).unwrap().1
    ));
}

#[test]
fn rule_lookup_keeps_user_rules_before_views_and_ordinary_views_before_materialized() {
    let catalog = fixture(1);
    let mut snapshot = catalog.snapshot().clone();
    let views = uqa_core::memory::RetainedValue::make_mut(&mut snapshot.definitions.views);
    for (name, kind) in [
        (
            "view_000",
            uqa_sql::catalog::view::StoredViewKind::Materialized,
        ),
        ("view_001", uqa_sql::catalog::view::StoredViewKind::View),
    ] {
        views
            .edit(&RelationIdentity::new("public", name), |view| {
                view.kind = kind;
                view.catalog_oids.as_mut().unwrap().rule = Some(80_000);
                Ok(())
            })
            .unwrap()
            .unwrap();
    }
    let catalog = CatalogReadView::new(snapshot);
    let services = CatalogServices::default();
    let output = crate::catalog::cache::RegtypeOutputCache::default();
    let context = services.context(&catalog, &output);
    let before = VIEW_ADDRESSES.get();
    let result =
        super::super::super::pg_get_ruledef_value(&context, &[uqa_core::Value::Int(80_000)])
            .unwrap();
    assert_eq!(
        result,
        uqa_core::Value::Str(
            "CREATE RULE rule_000 AS\n    ON UPDATE TO public.items DO NOTHING;".into()
        )
    );
    assert_eq!(VIEW_ADDRESSES.get(), before);
    assert_eq!(
        view_rule_by_oid(&catalog, 80_000).unwrap().0.name,
        "view_001"
    );
}

#[test]
fn a_later_missing_view_rule_identity_does_not_affect_an_earlier_match() {
    let catalog = fixture(2);
    let mut snapshot = catalog.snapshot().clone();
    uqa_core::memory::RetainedValue::make_mut(&mut snapshot.definitions.views)
        .edit(&RelationIdentity::new("public", "view_002"), |view| {
            view.catalog_oids.as_mut().unwrap().rule = None;
            Ok(())
        })
        .unwrap()
        .unwrap();
    let catalog = CatalogReadView::new(snapshot);
    assert!(view_rule_by_oid(&catalog, 90_003).is_some());
    assert!(view_by_oid(&catalog, 90_020).is_some());
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| view_rule_by_oid(
            &catalog, 90_013
        )))
        .is_err()
    );
}
