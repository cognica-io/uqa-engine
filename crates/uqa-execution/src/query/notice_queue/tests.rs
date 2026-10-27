//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn one_notice_admission_preserves_rejected_and_panicked_transfers() {
    let queue = NoticeQueue::default();
    let first = SQLNotice::notice("first")
        .with_detail("detail")
        .with_hint("hint");
    let original = first.message.as_ptr();
    queue.push(first);
    queue.push(SQLNotice::warning("second"));
    let rejected: Result<Option<(SQLNotice, ())>, &str> = queue.try_take_with(|notice| {
        assert_eq!(notice.message, "first");
        Err("no capacity")
    });
    assert_eq!(rejected.unwrap_err(), "no capacity");
    assert_eq!(queue.len(), 2);
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _: Result<Option<(SQLNotice, ())>, ()> = queue.try_take_with(|_| panic!("admission"));
    }))
    .is_err());
    assert_eq!(queue.len(), 2);
    let (first, payload) = queue.try_take_with(|_| Ok::<_, ()>(7)).unwrap().unwrap();
    assert_eq!(payload, 7);
    assert_eq!(first.message.as_ptr(), original);
    assert_eq!(first.detail.as_deref(), Some("detail"));
    assert_eq!(first.hint.as_deref(), Some("hint"));
    assert_eq!(queue.take(), [SQLNotice::warning("second")]);
    assert!(queue
        .try_take_with(|_| Err::<(), _>("must not run"))
        .unwrap()
        .is_none());
}

#[test]
fn one_notice_transfer_moves_each_payload_once_in_emission_order() {
    let queue = NoticeQueue::default();
    let mut addresses = Vec::new();
    for index in 0..2048 {
        let notice = SQLNotice::notice(format!("notice {index}"));
        addresses.push(notice.message.as_ptr());
        queue.push(notice);
    }
    let mut calls = 0;
    for (index, address) in addresses.into_iter().enumerate() {
        let (notice, ()) = queue
            .try_take_with(|_| {
                calls += 1;
                Ok::<_, ()>(())
            })
            .unwrap()
            .unwrap();
        assert_eq!(notice.message.as_ptr(), address);
        assert_eq!(notice.message, format!("notice {index}"));
        assert_eq!(queue.len(), 2047 - index);
    }
    assert_eq!(calls, 2048);
    assert!(queue.is_empty());
}

#[test]
fn queued_notices_keep_their_emission_time_filter() {
    let level = Arc::new(AtomicU8::new(message_levels::NOTICE));
    let queue = NoticeQueue::new(Arc::clone(&level));
    queue.push(SQLNotice::notice("retained"));
    level.store(message_levels::WARNING, Ordering::Release);
    queue.push(SQLNotice::notice("filtered"));
    queue.push(SQLNotice::new(uqa_sql::NoticeLevel::Info, "always sent"));
    assert_eq!(
        queue.snapshot(),
        [
            SQLNotice::notice("retained"),
            SQLNotice::new(uqa_sql::NoticeLevel::Info, "always sent"),
        ]
    );
    assert_eq!(
        queue
            .try_take_with(|_| Ok::<_, ()>(()))
            .unwrap()
            .unwrap()
            .0
            .message,
        "retained"
    );
    assert_eq!(
        queue.take(),
        [SQLNotice::new(uqa_sql::NoticeLevel::Info, "always sent")]
    );
}
