//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{StorageHandle, StorageName};
use crate::{read_control::StorageReadControl, StorageBackendError};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use uqa_core::memory::{Budgeted, BudgetedString, MemoryBudget};

fn name(control: &StorageReadControl, text: &str) -> StorageName {
    let mut value = BudgetedString::new(control.memory());
    value.push_str(text).unwrap();
    let (value, memory) = value.into_parts();
    let pointer = value.as_ptr();
    let value = StorageName::from_admitted(Budgeted::new(value, memory)).unwrap();
    assert_eq!(value.as_ptr(), pointer);
    value
}

#[test]
fn admitted_storage_names_share_original_buffers_until_the_last_handle_retires() {
    let control = StorageReadControl::with_limit(1 << 20);
    let original = name(&control, "공개.테이블");
    let pointer = original.as_ptr();
    let bytes = control.memory().used();
    let clone = allocation_counter::measure(|| {
        let copied = original.clone();
        assert_eq!(copied.as_ptr(), pointer);
        assert_eq!(control.memory().used(), bytes);
        drop(copied);
    });
    assert_eq!(clone.count_total, 0);
    let retained = original.clone();
    drop(original);
    control.cancellation().cancel();
    assert_eq!(retained.as_str(), "공개.테이블");
    assert_eq!(control.memory().used(), bytes);
    drop(retained);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn admitted_storage_names_reject_unrelated_factories_and_retire_failed_headers() {
    let control = StorageReadControl::with_limit(1024);
    let original = name(&control, "table");
    assert!(original.check_control(&control).is_ok());
    assert!(matches!(
        original.check_control(&StorageReadControl::with_limit(1024)),
        Err(StorageBackendError::Other(_))
    ));
    assert!(matches!(
        StorageName::from("table".to_owned()).check_control(&control),
        Err(StorageBackendError::Other(_))
    ));
    drop(original);
    assert_eq!(control.memory().used(), 0);
    let memory = MemoryBudget::new(0);
    let value = Budgeted::new(String::new(), memory.reserve(0).unwrap());
    assert!(StorageName::from_admitted(value).is_err());
    assert_eq!(memory.used(), 0);
}

struct Probe {
    memory: MemoryBudget,
    retired: Arc<AtomicUsize>,
    value: usize,
}

impl Drop for Probe {
    fn drop(&mut self) {
        assert!(self.memory.used() >= size_of::<Self>());
        self.retired.fetch_add(self.value, Ordering::SeqCst);
    }
}

#[test]
fn admitted_storage_handles_retire_payload_before_returning_the_box_allowance() {
    let memory = MemoryBudget::new(1024);
    let retired = Arc::new(AtomicUsize::new(0));
    let lease = memory.reserve(size_of::<Probe>()).unwrap();
    let mut handle = StorageHandle::from(Budgeted::new(
        Box::new(Probe {
            memory: memory.clone(),
            retired: retired.clone(),
            value: 1,
        }),
        lease,
    ));
    handle.value = 7;
    assert_eq!(memory.used(), size_of::<Probe>());
    assert_eq!(handle.as_ref().value, 7);
    drop(handle);
    assert_eq!(retired.load(Ordering::SeqCst), 7);
    assert_eq!(memory.used(), 0);
}
