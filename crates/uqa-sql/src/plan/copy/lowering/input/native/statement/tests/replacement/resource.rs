//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn nested(name: &str, depth: usize) -> Arc<Statement> {
    let mut source = crate::compile(&format!("SELECT {name}")).unwrap().remove(0);
    for _ in 0..depth {
        let Statement::Select(child) = source else {
            unreachable!()
        };
        let Statement::Select(mut parent) = crate::compile("SELECT 1").unwrap().remove(0) else {
            unreachable!()
        };
        parent.projections[0].expr = crate::ast::Expr::ScalarSubquery(child);
        source = Statement::Select(parent);
    }
    Arc::new(source)
}

#[test]
fn native_root_replacement_retires_wide_syntax_and_original_layouts_in_work_windows() {
    let budget = MemoryBudget::new(64 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let Statement::Select(mut syntax) = crate::compile("SELECT old_col").unwrap().remove(0) else {
        unreachable!()
    };
    syntax.projections = vec![syntax.projections[0].clone(); 2048];
    let destination = construct(&Arc::new(Statement::Select(syntax)), &budget, &work, &token);
    let mut next = construct(&statement("SELECT new_col"), &budget, &work, &token);
    // These descriptors belong to the discarded replacement header, not the destination root.
    layouts(&mut next, 64, 32, &control);
    let updates = Replacements {
        query: Some(QueryMetadataUpdate {
            root: Some(next.try_into().unwrap()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let mut reader = input(destination, updates, &budget, &work, &token, &token);
    let mut polls = 0;
    loop {
        let mut result = None;
        let allocation = allocation_counter::measure(|| result = Some(advance(&mut reader)));
        assert!(
            allocation.count_current >= -128,
            "unbounded release: {allocation:?}"
        );
        polls += 1;
        if let Some((plan, visited)) = result.unwrap().unwrap() {
            assert_eq!(visited, ["new_col"]);
            assert!(query(&plan).analyzed_output.is_none());
            assert_eq!(allocation_counter::measure(|| drop(plan)).count_total, 0);
            break;
        }
        assert!(polls < 100_000);
    }
    assert!(polls > 2048 + 64 * 32);
    drop(reader);
    assert_eq!((budget.used(), work.used()), (0, 0));
}

#[test]
fn nested_native_replacement_admits_new_depth_before_small_stack_retirement() {
    let original = nested("replace_me", 128);
    let replacement = nested("new_col", 512);
    let retained = (original.clone(), replacement.clone());
    std::thread::Builder::new()
        .stack_size(256 << 10)
        .spawn(move || {
            let budget = MemoryBudget::new(128 << 20);
            let work = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let destination = construct(&original, &budget, &work, &token);
            let next = construct(&replacement, &budget, &work, &token);
            let updates = Replacements {
                query: Some(QueryMetadataUpdate {
                    root: Some(next.try_into().unwrap()),
                    ..Default::default()
                }),
                at: Some("replace_me"),
                ..Default::default()
            };
            let (output, visited) = input(destination, updates, &budget, &work, &token, &token)
                .read(&mut std::io::empty())
                .unwrap();
            assert_eq!(visited, ["new_col"]);
            assert_eq!(allocation_counter::measure(|| drop(output)).count_total, 0);
            assert_eq!((budget.used(), work.used()), (0, 0));
        })
        .unwrap()
        .join()
        .unwrap();
    drop(retained);
}

#[test]
fn native_replacement_depth_does_not_accumulate_across_sibling_queries() {
    let budget = MemoryBudget::new(128 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let replacement = nested("new_col", 32);
    let mut changes = Vec::new();
    for width in [16, 256] {
        let Statement::Select(mut syntax) = crate::compile("SELECT (SELECT unchanged)")
            .unwrap()
            .remove(0)
        else {
            unreachable!()
        };
        syntax.projections = vec![syntax.projections[0].clone(); width];
        let Statement::Select(last) = crate::compile("SELECT replace_me").unwrap().remove(0) else {
            unreachable!()
        };
        syntax.projections.last_mut().unwrap().expr = crate::ast::Expr::ScalarSubquery(last);
        let destination = construct(&Arc::new(Statement::Select(syntax)), &budget, &work, &token);
        let next = construct(&replacement, &budget, &work, &token);
        let before = budget.used();
        let updates = Replacements {
            query: Some(QueryMetadataUpdate {
                root: Some(next.try_into().unwrap()),
                ..Default::default()
            }),
            at: Some("replace_me"),
            ..Default::default()
        };
        let (output, visited) = input(destination, updates, &budget, &work, &token, &token)
            .read(&mut std::io::empty())
            .unwrap();
        assert_eq!(visited.len(), width);
        assert_eq!(visited.last().unwrap(), "new_col");
        changes.push(budget.used() as i128 - before as i128);
        drop(output);
        assert_eq!((budget.used(), work.used()), (0, 0));
    }
    assert_eq!(
        changes[0], changes[1],
        "sibling width must not increase admitted replacement depth"
    );
}
