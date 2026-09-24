//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! An unlocked initial SQLite header read must use the same file as its decoded chunk map.

use super::*;

struct Reader(CompressedSQLiteFile);

impl Reader {
    fn open(path: &Path) -> Self {
        let name = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
        let mut file = CompressedSQLiteFile {
            base: ffi::sqlite3_file {
                pMethods: ptr::null(),
            },
            handle: ptr::null_mut(),
        };
        // SAFETY: the wrapper provides the complete VFS allocation and the NUL-terminated path remains alive through xOpen.
        let result = unsafe {
            vfs_open(
                ptr::null_mut(),
                name.as_ptr(),
                &raw mut file.base,
                ffi::SQLITE_OPEN_READONLY | ffi::SQLITE_OPEN_MAIN_DB,
                ptr::null_mut(),
            )
        };
        assert_eq!(result, ffi::SQLITE_OK);
        Self(file)
    }

    fn header(&mut self) -> [u8; 100] {
        let mut header = [0; 100];
        // SAFETY: the file was opened by this VFS and the output buffer has the exact requested length.
        let result = unsafe {
            IO_METHODS.xRead.unwrap()(&raw mut self.0.base, header.as_mut_ptr().cast(), 100, 0)
        };
        assert_eq!(result, ffi::SQLITE_OK);
        header
    }

    fn lock(&mut self) {
        // SAFETY: the wrapper retains the opened VFS handle throughout the callback.
        assert_eq!(
            unsafe { IO_METHODS.xLock.unwrap()(&raw mut self.0.base, SQLITE_LOCK_SHARED) },
            ffi::SQLITE_OK
        );
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        // SAFETY: this wrapper uniquely owns the opened VFS handle and closes it once.
        unsafe {
            IO_METHODS.xClose.unwrap()(&raw mut self.0.base);
        }
    }
}

#[rstest::rstest]
fn initial_header_uses_its_opened_generation_after_another_writer_compacts(
    #[values(false, true)] encrypted: bool,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("read-generation.db");
    let mut options = encrypted_options();
    if !encrypted {
        options.key = None;
    }
    register_database(&path, options.compression, options.key.as_deref()).unwrap();
    let mut writer_locks = FileLocks::open(&path, false).unwrap();
    let mut writer = ContainerFile::open(path.clone(), options).unwrap();
    writer.write_at(0, &[b'a'; 512]).unwrap();
    writer.flush().unwrap();
    let mut reader = Reader::open(&path);
    assert!(writer_locks.lock(SQLITE_LOCK_SHARED).unwrap());
    assert!(writer_locks.lock(SQLITE_LOCK_EXCLUSIVE).unwrap());
    let mut compacted = false;
    let mut latest = [b'a'; 100];
    for value in 1..=128_u8 {
        let previous_generation = writer.generation;
        writer.write_at(0, &[value; 512]).unwrap();
        writer.flush().unwrap();
        latest.fill(value);
        if writer.generation > previous_generation + 1 {
            compacted = true;
            break;
        }
    }
    assert!(
        compacted,
        "fixture must replace the pathname through real compaction"
    );
    writer_locks.unlock(SQLITE_LOCK_NONE).unwrap();
    // SQLite reads its first 100 header bytes before acquiring the first shared lock. Its decoded chunk map still describes the file selected by xOpen.
    assert_eq!(reader.header(), [b'a'; 100]);
    reader.lock();
    assert_eq!(reader.header(), latest);
}
