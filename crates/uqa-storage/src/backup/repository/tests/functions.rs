//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    image_fixture::{Image, Unit},
    *,
};
use std::{io::Write, path::Path};

const WORKSPACE: u64 = 4 << 20;
type Declarations = Vec<(String, BackupFunctionKind)>;

struct Selection {
    rows: Declarations,
    at: usize,
    positions: u64,
}

impl Selection {
    fn new(rows: Declarations) -> Self {
        Self {
            rows,
            at: 0,
            positions: 0,
        }
    }
}

impl BackupFunctionSource for Selection {
    fn advance(&mut self, maximum: usize) -> BackupResult<BackupSourceStep> {
        assert_eq!(maximum, 1);
        self.positions += 1;
        if self.positions % 2 == 1 {
            return Ok(BackupSourceStep::Pending);
        }
        if self.at == self.rows.len() {
            return Ok(BackupSourceStep::Complete);
        }
        self.at += 1;
        Ok(BackupSourceStep::Unit)
    }

    fn current(&self) -> Option<BackupFunctionReference<'_>> {
        let (name, kind) = self.rows.get(self.at.checked_sub(1)?)?;
        Some(BackupFunctionReference { name, kind: *kind })
    }

    fn statistics(&self) -> BackupSourceStatistics {
        BackupSourceStatistics {
            metadata_read_bytes: self.positions * 13,
            visited_positions: self.positions,
            ..BackupSourceStatistics::default()
        }
    }
}

fn capture_functions(
    repo: &LocalRepository,
    parent: Option<BackupId>,
    rows: Option<Declarations>,
    control: &StorageReadControl,
) -> BackupResult<(BackupRoot, BackupCaptureReport)> {
    let image = OwnedBackupImage::capture(control, || {
        Ok(Image::new(vec![Unit::record(42, b"provider record")]))
    })?;
    let image = if let Some(rows) = rows {
        image.with_function_references(Selection::new(rows), control)?
    } else {
        image
    };
    let mut job = repo.begin_image_backup(image, parent, WORKSPACE, WORKSPACE, control)?;
    while job.advance(1, 1024)? == BackupCaptureStep::Pending {}
    Ok((job.publish()?, job.report().unwrap()))
}

fn read(
    lease: &BackupLease<'_>,
    control: &StorageReadControl,
    window: usize,
) -> BackupResult<(Declarations, ImageDigest, usize)> {
    let mut reader = lease.function_references(control)?.unwrap();
    let mut steps = 0;
    let mut rows = Vec::new();
    loop {
        steps += 1;
        match reader.advance(window)? {
            BackupSourceStep::Unit => {
                let reference = reader.current().unwrap();
                rows.push((reference.name.to_owned(), reference.kind));
            }
            BackupSourceStep::Pending => assert!(reader.current().is_none()),
            BackupSourceStep::Complete => return Ok((rows, reader.expected_digest(), steps)),
        }
    }
}

fn contains_plaintext(path: &Path, needle: &[u8]) -> bool {
    if path.is_dir() {
        return std::fs::read_dir(path)
            .unwrap()
            .any(|entry| contains_plaintext(&entry.unwrap().path(), needle));
    }
    std::fs::read(path)
        .unwrap()
        .windows(needle.len())
        .any(|part| part == needle)
}

#[test]
fn full_incremental_and_reprotected_copies_preserve_only_the_selected_root_declarations() {
    use BackupFunctionKind::{AbsentTable, Aggregate, Scalar};
    let directory = tempfile::tempdir().unwrap();
    let control = control();
    let first_path = directory.path().join("first");
    let source = LocalRepository::create(&first_path, protected(), limits(), &control).unwrap();
    let name = "private_function_selection_87ab";
    let original = vec![(name.to_owned(), Scalar), (name.to_owned(), AbsentTable)];
    let replacement = vec![(name.to_owned(), Aggregate)];
    let (full, original_report) =
        capture_functions(&source, None, Some(original.clone()), &control).unwrap();
    assert_eq!(original_report.source.metadata_read_bytes, 6 * 13);
    let (same, report) = capture_functions(
        &source,
        Some(full.backup()),
        Some(original.clone()),
        &control,
    )
    .unwrap();
    assert_eq!(report.image.reused_units, 2);
    let (changed, report) = capture_functions(
        &source,
        Some(same.backup()),
        Some(replacement.clone()),
        &control,
    )
    .unwrap();
    assert_eq!(report.image.reused_units, 1);
    assert_ne!(changed.summary().image_digest, same.summary().image_digest);
    assert!(!contains_plaintext(&first_path, name.as_bytes()));
    source.remove_backup(full.backup(), &control).unwrap();
    source.collect_garbage(1000, WORKSPACE, &control).unwrap();
    assert_eq!(
        read(
            &source.open_backup(same.backup(), &control).unwrap(),
            &control,
            7
        )
        .unwrap()
        .0,
        original
    );
    let child = source.open_backup(changed.backup(), &control).unwrap();
    let expected = read(&child, &control, 3).unwrap().1;
    let destination_path = directory.path().join("second");
    let mode = Protection::Encrypted {
        domain: ProtectionId::new().unwrap(),
        key: BackupKey::new([17; 32]),
    };
    let destination =
        LocalRepository::create(&destination_path, mode.clone(), limits(), &control).unwrap();
    let mut copy = destination
        .begin_backup(changed.cut(), None, WORKSPACE, &control)
        .unwrap();
    copy.prepare_copy(&child, WORKSPACE, &control).unwrap();
    let copied = copy.publish(&control).unwrap();
    drop((copy, child));
    drop((source, destination));
    std::fs::remove_dir_all(first_path).unwrap();
    let destination = LocalRepository::open(&destination_path, mode, limits(), &control).unwrap();
    let lease = destination.open_backup(copied.backup(), &control).unwrap();
    lease.verify(WORKSPACE, &control).unwrap();
    assert_eq!(read(&lease, &control, 1).unwrap().0, replacement);
    assert_eq!(read(&lease, &control, 1024).unwrap().1, expected);
    assert!(!contains_plaintext(&destination_path, name.as_bytes()));
    drop(lease);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn long_shared_prefixes_and_split_utf8_resume_under_single_byte_windows() {
    use BackupFunctionKind::{AbsentScalar, Aggregate, Table};
    let directory = tempfile::tempdir().unwrap();
    let control = control();
    let repo = LocalRepository::create(
        directory.path().join("repo"),
        protected(),
        limits(),
        &control,
    )
    .unwrap();
    let common = "é가".repeat(2000);
    let rows = vec![
        (format!("{common}가"), Table),
        (format!("{common}가"), AbsentScalar),
        (format!("{common}각"), Aggregate),
    ];
    let (root, _) = capture_functions(&repo, None, Some(rows.clone()), &control).unwrap();
    let lease = repo.open_backup(root.backup(), &control).unwrap();
    let (decoded, expected, steps) = read(&lease, &control, 1).unwrap();
    assert_eq!(decoded, rows);
    let bytes = 9 + rows.iter().map(|(name, _)| 5 + name.len()).sum::<usize>();
    assert_eq!(steps, bytes + 1);
    let mut job = BackupFunctionDigestJob::new(Selection::new(rows.clone()), &control);
    let mut pending = 0;
    while job.advance(1, 1).unwrap().is_none() {
        pending += 1;
    }
    assert!(pending >= bytes);
    assert_eq!(job.advance(1, 1).unwrap(), Some(expected));
    let mut canonical = b"UQAFUN01".to_vec();
    for (name, kind) in &rows {
        canonical.push(*kind as u8);
        canonical.extend_from_slice(&u32::try_from(name.len()).unwrap().to_le_bytes());
        canonical.extend_from_slice(name.as_bytes());
    }
    canonical.push(0);
    assert_eq!(ImageDigest::of_bytes(&canonical), expected);
    let (short, _, wide_steps) = read(&lease, &control, 4096).unwrap();
    assert_eq!(short, rows);
    assert!(wide_steps < steps / 100);
    for cancel in [false, true] {
        let restricted = StorageReadControl::with_limit(if cancel { 100_000 } else { 8_192 });
        let mut reader = lease.function_references(&restricted).unwrap().unwrap();
        if cancel {
            assert_eq!(reader.advance(1).unwrap(), BackupSourceStep::Pending);
            restricted.cancellation().cancel();
        }
        let error = loop {
            if let Err(error) = reader.advance(1) {
                break error;
            }
        };
        assert!(matches!(
            error,
            BackupError::Memory(_) | BackupError::Cancelled(_)
        ));
        assert!(matches!(reader.advance(1), Err(BackupError::Finished)));
        assert_eq!(restricted.memory().used(), 0);
    }
}

#[test]
fn malformed_or_contradictory_declarations_fail_before_publication() {
    use BackupFunctionKind::{AbsentScalar, Scalar};
    let directory = tempfile::tempdir().unwrap();
    let control = control();
    let repo = LocalRepository::create(
        directory.path().join("repo"),
        protected(),
        limits(),
        &control,
    )
    .unwrap();
    for entries in [
        vec![("a", Scalar), ("a", Scalar)],
        vec![("a", Scalar), ("a", AbsentScalar)],
        vec![("z", Scalar), ("a", Scalar)],
        vec![("A", Scalar)],
        vec![("日　", Scalar)],
        vec![("", Scalar)],
        vec![("a\0b", Scalar)],
    ] {
        let rows = entries
            .into_iter()
            .map(|(name, kind)| (name.to_owned(), kind))
            .collect();
        assert!(matches!(
            capture_functions(&repo, None, Some(rows), &control),
            Err(BackupError::Invalid(_))
        ));
        assert!(repo
            .list_backups(WORKSPACE, &control)
            .unwrap()
            .next_backup()
            .unwrap()
            .is_none());
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn independently_encoded_function_sections_reject_invalid_versions_utf8_lengths_and_trailing_bytes()
{
    let directory = tempfile::tempdir().unwrap();
    let control = control();
    let repo = LocalRepository::create(
        directory.path().join("repo"),
        protected(),
        limits(),
        &control,
    )
    .unwrap();
    for (contents, kind) in [
        b"UQAFUN02\x00".as_slice(),
        b"UQAFUN01\x07\x00",
        b"UQAFUN01\x01\x02\x00\x00\x00\xc3x\x00",
        b"UQAFUN01\x01\x01\x00\x00\x00\xc3\x00",
        b"UQAFUN01\x01\xff\xff\xff\xff\x00",
        b"UQAFUN01\x01\x01\x00\x00\x00x",
        b"UQAFUN01\x00x",
    ]
    .into_iter()
    .map(|bytes| (bytes, UnitKind::FunctionReferences))
    .chain([(b"UQAFUN01\x00".as_slice(), UnitKind::Metadata)])
    {
        let mut operation = repo.begin_backup(cut(), None, WORKSPACE, &control).unwrap();
        let span = operation
            .write_object(FrameKind::Unit, &mut Cursor::new(contents), &control)
            .unwrap();
        let entry = InventoryEntry::new(
            UnitAddress {
                namespace: NamespaceId::from_bytes(1_u128.to_be_bytes()).unwrap(),
                generation: 1,
                family: 1,
                key: &[],
            },
            kind,
            contents.len() as u64,
            ImageDigest::of_bytes(contents),
            span,
            limits(),
            &control,
        )
        .unwrap();
        let mut inventory =
            InventoryWriter::new(Vec::new(), operation.cut().format, limits(), &control).unwrap();
        inventory.append(&entry).unwrap();
        let (encoding, summary) = inventory.finish().unwrap();
        let inventory = operation
            .write_object(FrameKind::Inventory, &mut Cursor::new(encoding), &control)
            .unwrap();
        if kind == UnitKind::Metadata {
            operation.prepare(inventory, summary, &control).unwrap();
            let root = operation.publish(&control).unwrap();
            let lease = repo.open_backup(root.backup(), &control).unwrap();
            assert!(lease.function_references(&control).unwrap().is_none());
            let mut provider = lease.read_image(WORKSPACE, &control).unwrap();
            assert_eq!(provider.advance_provider(1).unwrap(), BackupReadStep::Unit);
            assert_eq!(provider.current_unit().unwrap().kind(), UnitKind::Metadata);
            continue;
        }
        assert!(matches!(
            operation.prepare(inventory, summary, &control),
            Err(BackupError::Invalid(_) | BackupError::Unsupported(_))
        ));
        assert!(operation.publish(&control).is_err());
        assert!(repo.open_backup(operation.id(), &control).is_err());
    }
}

struct Verified;

impl BackupRestoreImporter for Verified {
    fn import(
        self,
        directory: &RestoreDirectory<'_>,
        backup: &BackupLease<'_>,
        request: crate::mvcc::DatabaseRestore,
        control: &StorageReadControl,
    ) -> BackupResult<()> {
        backup.verify(WORKSPACE, control)?;
        let mut marker = directory.create_file("database")?;
        marker.write_all(&request.target().as_bytes())?;
        marker.sync_all()?;
        Ok(())
    }

    fn recover(
        self,
        _: &RestoreDirectory<'_>,
        _: &BackupLease<'_>,
        _: crate::mvcc::DatabaseRestore,
        _: &StorageReadControl,
    ) -> BackupResult<()> {
        panic!("prepared operation reopened source input")
    }
}

#[test]
fn durable_prepared_receipts_keep_legacy_empty_and_populated_requirements_after_collection() {
    let name = "only_in_the_function_section_76a9";
    for protection in [Protection::Plaintext, protected()] {
        for declarations in [
            None,
            Some(Vec::new()),
            Some(vec![(name.to_owned(), BackupFunctionKind::Table)]),
        ] {
            let control = control();
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("repo");
            let db = directory.path().join("db");
            let repo =
                LocalRepository::create(&path, protection.clone(), limits(), &control).unwrap();
            let present = declarations.is_some();
            let (root, _) = capture_functions(&repo, None, declarations, &control).unwrap();
            let lease = repo.open_backup(root.backup(), &control).unwrap();
            let expected = lease
                .function_references(&control)
                .unwrap()
                .map(|reader| reader.expected_digest());
            assert_eq!(expected.is_some(), present);
            drop(lease);
            let mut operation = repo.begin_restore(root.backup(), &db, &control).unwrap();
            assert_eq!(operation.function_reference_digest(), expected);
            let request = operation.id();
            operation.prepare(Verified).unwrap();
            drop(operation);
            repo.remove_backup(root.backup(), &control).unwrap();
            repo.collect_garbage(100, WORKSPACE, &control).unwrap();
            assert_eq!(image_fixture::objects(&repo), (0, 0));
            drop(repo);
            assert!(!contains_plaintext(directory.path(), name.as_bytes()));
            let repo =
                LocalRepository::open(&path, protection.clone(), limits(), &control).unwrap();
            let mut operation = repo.resume_restore(request, &db, &control).unwrap();
            assert_eq!(operation.function_reference_digest(), expected);
            assert_eq!(operation.progress().unwrap(), BackupRestoreStep::Prepared);
            operation.publish().unwrap();
            drop(operation);
            assert_eq!(
                repo.resume_restore(request, &db, &control)
                    .unwrap()
                    .function_reference_digest(),
                expected
            );
            assert_eq!(control.memory().used(), 0);
        }
    }
}
