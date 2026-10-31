//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn producer_admission_releases_partial_destinations_and_reports_close_failures() {
    let mut rejected = 0;
    let mut admitted = 0;
    for available in [0, 64, 128, 256, 512, 1024, 2048, 4096] {
        let budget = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let mut scope = CteScope::<()>::empty_with_control(&control).unwrap();
        let (input, counts) = input(&scope, "admission", true, &Arc::default());
        let before = budget.used();
        let occupied = budget.reserve(budget.available() - available).unwrap();
        match scope.insert_producer_with_control("admitted", input, 1, &control) {
            Ok(()) => {
                admitted += 1;
                assert!(scope.rows.contains_key("admitted"));
                scope.finish_cte_producers(Ok(())).unwrap_err();
            }
            Err(error) => {
                rejected += 1;
                assert_eq!(error.sqlstate(), Some("53200"));
                assert!(error.detail().unwrap().contains("admission hint"));
                assert!(scope.rows.is_empty());
                assert_eq!(budget.used(), before + occupied.bytes());
            }
        }
        closed(&counts, 0);
        drop((occupied, scope));
        assert_eq!(budget.used(), 0);
    }
    assert!(rejected >= 3 && admitted > 0);
}

#[test]
fn producer_admission_unlocks_its_list_before_running_failure_callbacks() {
    struct Reentrant {
        producers: Arc<Produced<ProducerList>>,
        closes: Arc<AtomicUsize>,
    }
    impl SQLRowSource<PhysicalRow> for Reentrant {
        fn next_row(&mut self) -> Result<Option<PhysicalRow>, SQLError> {
            panic!("must not read");
        }
        fn close(&mut self) -> Result<(), SQLError> {
            assert!(
                self.producers.0.try_lock().is_some(),
                "admission held the owner lock during close"
            );
            assert_eq!(self.closes.fetch_add(1, Ordering::SeqCst), 0);
            Ok(())
        }
    }
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for retired in [false, true] {
        let mut scope = CteScope::<()>::empty_with_control(&control).unwrap();
        if retired {
            scope.finish_cte_producers(Ok(())).unwrap();
        }
        let closes = Arc::new(AtomicUsize::new(0));
        let input = Box::new(PhysicalRowIteratorScan::from_stream(
            RowSchema::default(),
            SQLRowStream::from_source(Reentrant {
                producers: scope.producers.list().unwrap(),
                closes: Arc::clone(&closes),
            }),
        ));
        let occupied = (!retired).then(|| budget.reserve(budget.available()).unwrap());
        let error = scope
            .insert_producer_with_control("reentrant", input, 1, &control)
            .unwrap_err();
        assert_eq!(closes.load(Ordering::SeqCst), 1);
        if retired {
            assert!(error.to_string().contains("owner has ended"));
        } else {
            assert_eq!(error.sqlstate(), Some("53200"));
        }
        drop((occupied, scope));
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn scoped_producer_owners_preserve_parent_state_and_last_reader_leases() {
    let budget = MemoryBudget::new(1 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let mut scope = CteScope::<()>::empty_with_control(&control).unwrap();
    let parent = scope.producers.list().unwrap();
    let before = budget.used();
    for token in [&original, &invoking] {
        token.cancel();
        let error = scope
            .with_cte_producers_with_control(true, &control, |_| -> Result<(), SQLError> {
                panic!("rejected constructor must not execute");
            })
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert!(Arc::ptr_eq(&parent, &scope.producers.list().unwrap()));
        assert_eq!(budget.used(), before);
        token.reset();
    }
    let (input, counts) = input(&scope, "reader", false, &Arc::default());
    scope
        .insert_producer_with_control("reader", input, 1, &control)
        .unwrap();
    let reader = scope.rows.get("reader").unwrap().clone();
    drop((parent, scope));
    closed(&counts, 0);
    assert!(
        budget.used() > 0,
        "live reader must retain the prefix owner"
    );
    drop(reader);
    assert_eq!(budget.used(), 0);
}

#[test]
fn rejected_retained_wrapper_reports_both_input_and_cte_cleanup_errors() {
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut scope = CteScope::<()>::empty_with_control(&control).unwrap();
    let parent = scope.producers.list().unwrap();
    let mut occupied = None;
    let mut counts = None;
    let error = scope
        .retain_cte_producers_with_control(true, &control, |scope| {
            let (producer, producer_counts) = input(scope, "producer", true, &Arc::default());
            scope.insert_producer_with_control("producer", producer, 1, &control)?;
            let (consumer, consumer_counts) = input(scope, "consumer", true, &Arc::default());
            counts = Some((producer_counts, consumer_counts));
            occupied = Some(budget.reserve(budget.available()).unwrap());
            Ok(Some(consumer))
        })
        .err()
        .unwrap();
    assert_eq!(error.sqlstate(), Some("53200"));
    let detail = error.detail().unwrap();
    assert!(detail.contains("producer hint"), "{detail}");
    assert!(detail.contains("consumer hint"), "{detail}");
    assert!(Arc::ptr_eq(&parent, &scope.producers.list().unwrap()));
    let (producer, consumer) = counts.unwrap();
    closed(&producer, 0);
    closed(&consumer, 0);
    drop((occupied, parent, scope));
    assert_eq!(budget.used(), 0);
}
