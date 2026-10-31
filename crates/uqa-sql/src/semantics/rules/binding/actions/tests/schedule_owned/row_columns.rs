//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::semantics::rules::analysis::relation_rule_row_columns_with_control;

#[test]
fn event_row_columns_keep_both_images_and_lexical_masking_with_exact_owners() {
    let catalog = super::super::owned::catalog_fixture();
    for (sql, expected) in [
        (
            "CREATE RULE r AS ON UPDATE TO items WHERE OLD.a>0 DO ALSO SELECT NEW.b,OLD.a",
            Some(vec!["a", "b"]),
        ),
        (
            "CREATE RULE r AS ON UPDATE TO items DO ALSO SELECT new.a FROM (SELECT 1 AS a) AS new",
            Some(vec![]),
        ),
        (
            "CREATE RULE r AS ON UPDATE TO items DO ALSO SELECT NEW",
            None,
        ),
        (
            "CREATE RULE r AS ON UPDATE TO items DO ALSO SELECT OLD.*",
            None,
        ),
    ] {
        let rules = Rules::new(&[sql]);
        let context = RuleAnalysisContext {
            rules: &rules,
            ..catalog.analysis()
        };
        let memory = MemoryBudget::new(1 << 20);
        let sibling = memory.reserve(37).unwrap();
        let token = CancellationToken::new();
        let control = ProductionControl::new(&memory, &token, &token);
        let mut output = None;
        let measured = allocation_counter::measure(|| {
            output = relation_rule_row_columns_with_control(
                context,
                "items",
                RuleEvent::Update,
                &control,
            )
            .unwrap();
        });
        assert_eq!(
            output
                .as_ref()
                .map(|value| value.iter().map(String::as_str).collect::<Vec<_>>()),
            expected,
            "{sql}"
        );
        assert_eq!(
            usize::try_from(measured.bytes_current).unwrap(),
            memory.used() - sibling.bytes(),
            "{sql}: {measured:?}"
        );
        drop(output);
        assert_eq!(memory.used(), sibling.bytes());
        drop(sibling);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn event_row_names_reject_before_copying_and_preserve_sibling_allowances() {
    let catalog = super::super::owned::catalog_fixture();
    let mut rules = Rules::new(&["CREATE RULE r AS ON INSERT TO items DO ALSO SELECT NEW.a,NEW.b"]);
    let rule = Arc::make_mut(
        Arc::make_mut(&mut rules.0)
            .get_mut(&RelationIdentity::new("public", "items"))
            .unwrap(),
    )
    .get_mut("r")
    .unwrap();
    let Statement::Select(action) = &mut rule.definition.actions[0] else {
        panic!("SELECT")
    };
    action.projections[1].expr = Expr::QualifiedColumn {
        qualifier: "new".into(),
        column: "x".repeat(4 << 20).into(),
    };
    let context = RuleAnalysisContext {
        rules: &rules,
        ..catalog.analysis()
    };
    let memory = MemoryBudget::new(128 << 10);
    let sibling = memory.reserve(37).unwrap();
    let statement = CancellationToken::new();
    let session = CancellationToken::new();
    let control = ProductionControl::new(&memory, &statement, &session);
    let measured = allocation_counter::measure(|| {
        assert_eq!(
            relation_rule_row_columns_with_control(context, "items", RuleEvent::Insert, &control)
                .unwrap_err()
                .sqlstate(),
            Some("53200")
        );
    });
    assert!(measured.bytes_total < 256 << 10, "{measured:?}");
    assert_eq!(memory.used(), sibling.bytes());
    for invoking in [false, true] {
        let statement = CancellationToken::new();
        let session = CancellationToken::new();
        if invoking {
            session.cancel();
        } else {
            statement.cancel();
        }
        let control = ProductionControl::new(&memory, &statement, &session);
        assert_eq!(
            relation_rule_row_columns_with_control(context, "items", RuleEvent::Insert, &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(memory.used(), sibling.bytes());
    }
    drop(sibling);
    assert_eq!(memory.used(), 0);
}
