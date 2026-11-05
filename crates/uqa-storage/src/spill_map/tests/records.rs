//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected roots contain only live keys and expose immutable encoded windows.

use super::*;

#[test]
fn resident_removal_prunes_keys_without_changing_retained_generations() {
    let control = StorageReadControl::with_limit(8 << 20);
    let mut live = Map::new(control.memory(), 4 << 20);
    for key in 0..4096 {
        live.insert(key, key as u64, Some(&control)).unwrap();
    }
    assert!(!live.is_spilled());
    let retained = live.clone();
    for key in 0..4096 {
        if ![17, 2048, 4095].contains(&key) {
            live.remove(key, Some(&control)).unwrap();
        }
    }
    let Root::Memory(root) = &live.root else {
        panic!("resident fixture");
    };
    assert_eq!(
        root.len(),
        3,
        "removed keys must not remain as hidden scan positions"
    );
    assert_eq!(entries(&live), [(17, 17), (2048, 2048), (4095, 4095)]);
    let empty = MemoryBudget::new(0);
    assert_eq!(live.next_key(None, &empty).unwrap(), Some(17));
    assert_eq!(live.next_key(Some(17), &empty).unwrap(), Some(2048));
    assert_eq!(live.next_key(Some(2048), &empty).unwrap(), Some(4095));
    assert_eq!(live.next_key(Some(4095), &empty).unwrap(), None);
    assert_eq!(live.last().unwrap().map(|(key, _)| key), Some(4095));
    assert_eq!(
        entries(&retained),
        (0..4096).map(|key| (key, key as u64)).collect::<Vec<_>>()
    );
    for key in [17, 2048, 4095] {
        live.remove(key, Some(&control)).unwrap();
    }
    let Root::Memory(root) = &live.root else {
        panic!("resident fixture");
    };
    assert_eq!(root.len(), 0);
    assert!(live.last().unwrap().is_none());
    assert!(live.next(None).unwrap().is_none());
    drop((live, retained));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn denied_resident_removal_keeps_both_roots_and_the_selected_value() {
    let control = StorageReadControl::with_limit(64 << 10);
    let files = crate::temporary_file::FileBudget::new(0);
    let mut live = Map::with_file_budget(control.memory(), 48 << 10, &files);
    for key in 0..128 {
        live.insert(key, key as u64, Some(&control)).unwrap();
    }
    let retained = live.clone();
    assert!(!live.is_spilled());
    let held = control
        .memory()
        .reserve(control.memory().available())
        .unwrap();
    let used = control.memory().used();
    assert!(live.remove(17, Some(&control)).is_err());
    assert_eq!(control.memory().used(), used);
    assert_eq!(files.usage().0, 0);
    assert_eq!(*live.get(17).unwrap().unwrap(), 17);
    assert_eq!(live.len(), 128);
    assert_eq!(entries(&live), entries(&retained));
    drop(held);
    live.remove(17, Some(&control)).unwrap();
    assert!(!live.contains_key(17).unwrap());
    assert!(retained.contains_key(17).unwrap());
    drop((live, retained));
    assert_eq!(control.memory().used(), 0);
}

struct Encoded(Vec<u8>);

impl Record for Encoded {
    fn memory_bytes(&self) -> StorageBackendResult<usize> {
        Ok(size_of::<Self>() + self.0.capacity())
    }
    fn encoded_bytes(&self) -> StorageBackendResult<usize> {
        Ok(self.0.len())
    }
    fn encode(&self, bytes: &mut Vec<u8>) {
        bytes.extend_from_slice(&self.0);
    }
    fn decode(_: &[u8]) -> StorageBackendResult<Self> {
        panic!("encoded windows must not decode the record")
    }
}

#[test]
fn spilled_record_windows_preserve_retained_bytes_after_replacement_without_payload_allocation() {
    let source = StorageReadControl::with_limit(8 << 20);
    let invoking = StorageReadControl::with_limit(0);
    let expected = (0..(1 << 20) + 13)
        .map(|i| ((i * 17 + 3) % 251) as u8)
        .collect::<Vec<_>>();
    let mut map = Map::new(source.memory(), 0);
    map.insert(7, Encoded(expected.clone()), Some(&source))
        .unwrap();
    let mut successor = map.clone();
    successor
        .insert(7, Encoded(vec![42; expected.len()]), Some(&source))
        .unwrap();
    let RecordRead::Spilled(record) = map.read_record(7, &invoking).unwrap().unwrap() else {
        panic!("spilled fixture");
    };
    assert_eq!(record.len(), expected.len());
    let held = source
        .memory()
        .reserve(source.memory().available())
        .unwrap();
    let mut output = vec![0; 8192];
    let allocation = allocation_counter::measure(|| {
        let mut offset = 0;
        while offset < expected.len() {
            let count = record.read_at(offset, &mut output, &invoking).unwrap();
            assert_eq!(count, 4096.min(expected.len() - offset));
            assert_eq!(&output[..count], &expected[offset..offset + count]);
            offset += count;
        }
        for offset in [0, 1, 1023, 4095, expected.len() - 1, expected.len()] {
            let count = record
                .read_at(offset, &mut output[..17], &invoking)
                .unwrap();
            assert_eq!(&output[..count], &expected[offset..offset + count]);
        }
    });
    assert_eq!(allocation.bytes_total, 0);
    assert_eq!(invoking.memory().used(), 0);
    assert!(record
        .read_at(record.len() + 1, &mut output, &invoking)
        .is_err());
    assert!(record.array::<8>(record.len() - 1, &invoking).is_err());
    invoking.cancellation().cancel();
    assert!(matches!(
        record.read_at(record.len(), &mut [], &invoking),
        Err(StorageBackendError::Cancelled(_))
    ));
    invoking.cancellation().reset();
    assert_eq!(record.array::<1>(0, &invoking).unwrap(), [expected[0]]);
    let RecordRead::Spilled(current) = successor.read_record(7, &invoking).unwrap().unwrap() else {
        panic!("spilled successor");
    };
    assert_eq!(current.array::<8>(0, &invoking).unwrap(), [42; 8]);
    assert!(map.read_record(8, &invoking).unwrap().is_none());
    drop((held, successor, map));
    assert_eq!(source.memory().used(), 0);
}
