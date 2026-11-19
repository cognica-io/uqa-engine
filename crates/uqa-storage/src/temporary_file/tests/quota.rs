//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn physical_allowance_follows_files_and_reopened_handles() {
    type File = BlockTemporaryFile<16>;
    let extent = File::physical_len_for(1).unwrap();
    let budget = FileBudget::new(extent * 2);
    let mut first = File::with_file_budget(&budget).unwrap();
    let mut second = File::with_file_budget(&budget).unwrap();
    first.write_all(b"first").unwrap();
    let alias = first.reopen().unwrap();
    second.write_all(&[6; 16]).unwrap();
    let before = second.block_io_counts().1;
    let error = second.set_len(17).unwrap_err();
    assert!(error.get_ref().unwrap().is::<FileLimit>());
    assert_eq!(
        second.block_io_counts().1,
        before,
        "reject before physical writes"
    );
    assert_eq!(budget.usage(), (extent * 2, extent * 2));
    drop(first);
    assert_eq!(budget.usage().0, extent * 2);
    drop(alias);
    second.set_len(17).unwrap();
    assert_eq!(budget.usage().0, extent * 2);
    second.set_len(0).unwrap();
    assert_eq!(budget.usage().0, 0);
}

#[test]
fn failed_truncation_retains_reserved_ciphertext_until_reclaimed() {
    type File = BlockTemporaryFile<16>;
    let extent = File::physical_len_for(1).unwrap();
    let budget = FileBudget::new(extent * 2);
    let mut file = File::with_file_budget(&budget).unwrap();
    file.write_all(&[7; 16]).unwrap();
    file.fail_write_after(3);
    file.owner.lock().faults.fail_truncate = true;
    assert!(file.write_all(b"x").is_err());
    assert_eq!(budget.usage().0, extent * 2);
    assert_eq!(file.metadata().unwrap().len(), 16);
    file.set_len(0).unwrap();
    assert_eq!(budget.usage().0, 0);
    file.write_all(b"new").unwrap();
    drop(file);
    assert_eq!(budget.usage().0, 0);
}

#[test]
fn failed_growth_does_not_release_another_files_allowance() {
    type File = BlockTemporaryFile<16>;
    let extent = File::physical_len_for(1).unwrap();
    let budget = FileBudget::new(extent);
    let mut first = File::with_file_budget(&budget).unwrap();
    let mut second = File::with_file_budget(&budget).unwrap();
    first.write_all(b"first").unwrap();
    assert!(second.write_all(b"second").is_err());
    assert_eq!(budget.usage().0, extent);
    assert_eq!(std::fs::metadata(second.path()).unwrap().len(), 0);
    drop(first);
    second.write_all(b"second").unwrap();
    drop(second);
    assert_eq!(budget.usage().0, 0);
}
