//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn packed_root(
    repository: &LocalRepository,
    control: &StorageReadControl,
    duplicate: bool,
) -> BackupResult<BackupRoot> {
    let mut operation = repository.begin_backup(cut(), None, 8 * 1024 * 1024, control)?;
    let mut pack = operation.begin_pack(control)?;
    let mut ranges = vec![None; 512];
    for index in (0..512).rev() {
        ranges[index] = Some(pack.append(
            FrameKind::Unit,
            &mut Cursor::new([index as u8; 32]),
            control,
        )?);
    }
    let object = pack.finish(control)?;
    let mut inventory = InventoryWriter::new(Vec::new(), cut().format, limits(), control)?;
    for index in 0..512 {
        let source = if duplicate && index == 511 { 0 } else { index };
        let key = (index as u64).to_be_bytes();
        let entry = InventoryEntry::new(
            UnitAddress {
                namespace: NamespaceId::from_bytes([3; 16])?,
                generation: 1,
                family: 1,
                key: &key,
            },
            UnitKind::Record,
            32,
            ImageDigest::of_bytes(&[source as u8; 32]),
            object.span(ranges[source].unwrap())?,
            limits(),
            control,
        )?;
        inventory.append(&entry)?;
    }
    let (bytes, summary) = inventory.finish()?;
    assert_eq!(summary.units, 512);
    let inventory =
        operation.write_object(FrameKind::Inventory, &mut Cursor::new(bytes), control)?;
    operation.prepare(inventory, summary, control)?;
    operation.publish(control)
}

#[test]
fn packed_inventory_verifies_under_fixed_memory_and_encrypted_workspace_limits() {
    let directory = tempfile::tempdir().unwrap();
    let control = StorageReadControl::with_limit(128 * 1024);
    let repository = LocalRepository::create(
        directory.path().join("backup"),
        protected(),
        limits(),
        &control,
    )
    .unwrap();
    let root = packed_root(&repository, &control, false).unwrap();
    let lease = repository.open_backup(root.backup(), &control).unwrap();
    lease.verify(8 * 1024 * 1024, &control).unwrap();
    assert_eq!(
        repository.objects.entries().unwrap().count(),
        2,
        "512 small units share one data pack"
    );
    assert!(control.memory().peak() <= 128 * 1024);
    assert_eq!(control.memory().used(), 0);
    assert!(matches!(
        lease.verify(0, &control),
        Err(BackupError::Limit {
            resource: "verification workspace",
            ..
        })
    ));
    assert_eq!(control.memory().used(), 0);
    lease.verify(8 * 1024 * 1024, &control).unwrap();
}

#[test]
fn nonadjacent_duplicate_ranges_are_rejected_before_publication() {
    let directory = tempfile::tempdir().unwrap();
    let control = control();
    let repository = LocalRepository::create(
        directory.path().join("backup"),
        protected(),
        limits(),
        &control,
    )
    .unwrap();
    assert!(matches!(
        packed_root(&repository, &control, true),
        Err(BackupError::Invalid("duplicate object frame range"))
    ));
    assert_eq!(repository.roots.entries().unwrap().count(), 0);
}

#[test]
fn dropped_and_failed_packs_terminate_capture() {
    let directory = tempfile::tempdir().unwrap();
    let control = control();
    let repository = LocalRepository::create(
        directory.path().join("backup"),
        protected(),
        limits(),
        &control,
    )
    .unwrap();
    let mut operation = repository
        .begin_backup(cut(), None, 1024 * 1024, &control)
        .unwrap();
    drop(operation.begin_pack(&control).unwrap());
    assert!(matches!(
        operation.begin_pack(&control),
        Err(BackupError::Finished)
    ));
    let mut operation = repository
        .begin_backup(cut(), None, 1024 * 1024, &control)
        .unwrap();
    let mut pack = operation.begin_pack(&control).unwrap();
    assert!(pack
        .append(FrameKind::Root, &mut Cursor::new(b"bad"), &control)
        .is_err());
    assert!(matches!(
        pack.append(FrameKind::Unit, &mut Cursor::new(b"x"), &control),
        Err(BackupError::Finished)
    ));
    assert!(matches!(pack.finish(&control), Err(BackupError::Finished)));
    assert!(matches!(
        operation.publish(&control),
        Err(BackupError::Finished)
    ));
}

#[test]
fn payload_reads_do_not_hold_repository_admission() {
    struct Reader<'a>(
        &'a LocalRepository,
        &'a StorageReadControl,
        Cursor<&'static [u8]>,
    );
    impl Read for Reader<'_> {
        fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
            let _admission = directory::lock(
                directory::open_file(&self.0.root, "admission", true).unwrap(),
                false,
                self.1,
            )
            .expect("repository gate is free during source reads");
            self.2.read(output)
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let control = control();
    let repository = LocalRepository::create(
        directory.path().join("backup"),
        protected(),
        limits(),
        &control,
    )
    .unwrap();
    let mut operation = repository
        .begin_backup(cut(), None, 1024 * 1024, &control)
        .unwrap();
    let mut source = Reader(&repository, &control, Cursor::new(b"source progress"));
    operation
        .write_object(FrameKind::Unit, &mut source, &control)
        .unwrap();
}

#[test]
fn immutable_pack_ranges_cannot_be_attached_to_another_object() {
    let directory = tempfile::tempdir().unwrap();
    let control = control();
    let repository = LocalRepository::create(
        directory.path().join("backup"),
        protected(),
        limits(),
        &control,
    )
    .unwrap();
    let mut operation = repository
        .begin_backup(cut(), None, 1024 * 1024, &control)
        .unwrap();
    let mut first = operation.begin_pack(&control).unwrap();
    let range = first
        .append(FrameKind::Unit, &mut Cursor::new(b"first"), &control)
        .unwrap();
    first.finish(&control).unwrap();
    let mut second = operation.begin_pack(&control).unwrap();
    second
        .append(FrameKind::Unit, &mut Cursor::new(b"second"), &control)
        .unwrap();
    assert!(matches!(
        second.finish(&control).unwrap().span(range),
        Err(BackupError::Invalid("foreign pack range"))
    ));
}
