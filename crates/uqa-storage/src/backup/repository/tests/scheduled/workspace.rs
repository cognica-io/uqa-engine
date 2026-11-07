//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::temporary_file::TemporaryFile;
use std::io::Write;

struct ImageWorkspace {
    image: Image,
    _file: TemporaryFile,
}

impl BackupImageSource for ImageWorkspace {
    fn cut(&self) -> BackupCut {
        self.image.cut()
    }
    fn advance(&mut self, maximum: usize) -> BackupResult<BackupSourceStep> {
        self.image.advance(maximum)
    }
    fn current_unit(&self) -> Option<BackupSourceUnit<'_>> {
        self.image.current_unit()
    }
    fn read_current(&mut self, output: &mut [u8]) -> BackupResult<usize> {
        self.image.read_current(output)
    }
    fn statistics(&self) -> BackupSourceStatistics {
        self.image.statistics()
    }
}

#[test]
fn scheduled_capture_source_and_repository_share_one_physical_workspace_limit() {
    let directory = tempfile::tempdir().unwrap();
    let limit = TemporaryFile::physical_len_for(16 * 1024 + 1).unwrap();
    let control = control().with_temporary_limit(limit);
    let repository = repository(directory.path(), &control);
    let files = control.file_budget().unwrap();
    let mut file = TemporaryFile::with_file_budget(files).unwrap();
    let path = file.path().to_path_buf();
    file.write_all(&[0; 16 * 1024]).unwrap();
    file.write_all(&[0]).unwrap();
    assert_eq!(files.usage().0, limit);
    let source = ImageWorkspace {
        image: Image::new(vec![]),
        _file: file,
    };
    let result = repository.begin_image_backup(source, None, 1 << 20, limit, &control);
    assert!(
        matches!(result, Err(BackupError::Limit { resource: "capture workspace", limit: found, required }) if found == limit && required > limit)
    );
    assert_eq!(files.usage(), (0, limit));
    assert!(!path.exists());
    assert_eq!(control.memory().used(), 0);
    let mut job = repository
        .begin_image_backup(Image::new(vec![]), None, 1 << 20, limit, &control)
        .unwrap();
    finish(&mut job, 128);
    job.publish().unwrap();
}

#[test]
fn scheduled_capture_rejects_a_source_workspace_that_exceeds_its_declared_limit() {
    let directory = tempfile::tempdir().unwrap();
    let control = control().with_temporary_limit(1 << 20);
    let repository = repository(directory.path(), &control);
    let source = Image::new(vec![]);
    let dropped = source.dropped.clone();
    assert!(matches!(
        repository.begin_image_backup(source, None, 1 << 20, 1 << 19, &control),
        Err(BackupError::Invalid(
            "source temporary limit exceeds capture workspace limit"
        ))
    ));
    assert!(dropped.load(Ordering::SeqCst));
    assert_eq!(control.memory().used(), 0);
    let mut job = repository
        .begin_image_backup(Image::new(vec![]), None, 1 << 20, 2 << 20, &control)
        .unwrap();
    finish(&mut job, 128);
    job.publish().unwrap();
}
