//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `SQLite` VFS callbacks, temporary paths, and lock-file lifecycle.

use super::{
    c_char, c_int, ffi, fill_random, fs, invalid_data, normalize_path, options_for_path, ptr,
    sync_parent_directory, AtomicOrdering, AtomicU64, CStr, CompressedSQLiteFile, Duration,
    FileHandle, FileLocks, Path, PathBuf, SystemTime, VfsFile, IO_METHODS, UNIX_EPOCH,
};

pub(super) unsafe extern "C" fn vfs_open(
    _vfs: *mut ffi::sqlite3_vfs,
    name: ffi::sqlite3_filename,
    file: *mut ffi::sqlite3_file,
    flags: c_int,
    out_flags: *mut c_int,
) -> c_int {
    let path = if name.is_null() {
        match temp_path() {
            Ok(path) => path,
            Err(_) => return ffi::SQLITE_CANTOPEN,
        }
    } else {
        match unsafe { CStr::from_ptr(name) }.to_str() {
            Ok(path) => PathBuf::from(path),
            Err(_) => return ffi::SQLITE_CANTOPEN,
        }
    };
    let normalized = match normalize_path(&path) {
        Ok(path) => PathBuf::from(path),
        Err(_) => return ffi::SQLITE_CANTOPEN,
    };
    let Ok(options) = options_for_path(&normalized) else {
        return ffi::SQLITE_CANTOPEN;
    };
    let read_only = flags & ffi::SQLITE_OPEN_READONLY != 0;
    let delete_on_close = flags & ffi::SQLITE_OPEN_DELETEONCLOSE != 0 || name.is_null();
    if flags & ffi::SQLITE_OPEN_CREATE == 0 && !normalized.exists() {
        return ffi::SQLITE_CANTOPEN;
    }
    let open_result = FileLocks::open(&normalized).and_then(|locks| {
        VfsFile::open(normalized, options, flags, read_only).map(|container| (container, locks))
    });
    let compressed = file.cast::<CompressedSQLiteFile>();
    unsafe {
        ptr::write(
            compressed,
            CompressedSQLiteFile {
                base: ffi::sqlite3_file {
                    pMethods: ptr::null(),
                },
                handle: ptr::null_mut(),
            },
        );
    }
    match open_result {
        Ok((container, locks)) => {
            let handle = Box::new(FileHandle {
                file: container,
                locks,
                read_only,
                delete_on_close,
            });
            unsafe {
                (*compressed).base.pMethods = &raw const IO_METHODS;
                (*compressed).handle = Box::into_raw(handle);
                if !out_flags.is_null() {
                    *out_flags = flags;
                }
            }
            ffi::SQLITE_OK
        }
        Err(_) => ffi::SQLITE_CANTOPEN,
    }
}

pub(super) unsafe extern "C" fn vfs_delete(
    _vfs: *mut ffi::sqlite3_vfs,
    name: *const c_char,
    sync_dir: c_int,
) -> c_int {
    if name.is_null() {
        return ffi::SQLITE_OK;
    }
    let Ok(path) = (unsafe { CStr::from_ptr(name) }).to_str() else {
        return ffi::SQLITE_IOERR_DELETE;
    };
    let normalized = match normalize_path(Path::new(path)) {
        Ok(path) => PathBuf::from(path),
        Err(_) => return ffi::SQLITE_IOERR_DELETE,
    };
    let Ok(namespace_changed) = remove_file_and_locks(&normalized) else {
        return ffi::SQLITE_IOERR_DELETE;
    };
    if sync_dir != 0 && namespace_changed && sync_parent_directory(&normalized).is_err() {
        return ffi::SQLITE_IOERR_DIR_FSYNC;
    }
    ffi::SQLITE_OK
}

pub(super) fn remove_file_and_locks(path: &Path) -> std::io::Result<bool> {
    let locks = super::locking::lock_paths(path);
    let mut changed = false;
    for path in std::iter::once(path).chain(locks.iter().map(PathBuf::as_path)) {
        match fs::remove_file(path) {
            Ok(()) => changed = true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(changed)
}

pub(super) unsafe extern "C" fn vfs_access(
    _vfs: *mut ffi::sqlite3_vfs,
    name: *const c_char,
    _flags: c_int,
    out: *mut c_int,
) -> c_int {
    if name.is_null() || out.is_null() {
        return ffi::SQLITE_IOERR_ACCESS;
    }
    let Ok(path) = (unsafe { CStr::from_ptr(name) }).to_str() else {
        return ffi::SQLITE_IOERR_ACCESS;
    };
    let Ok(normalized) = normalize_path(Path::new(path)) else {
        return ffi::SQLITE_IOERR_ACCESS;
    };
    unsafe {
        *out = i32::from(Path::new(&normalized).exists());
    }
    ffi::SQLITE_OK
}

pub(super) unsafe extern "C" fn vfs_full_pathname(
    _vfs: *mut ffi::sqlite3_vfs,
    name: *const c_char,
    output_len: c_int,
    output: *mut c_char,
) -> c_int {
    if name.is_null() || output.is_null() || output_len <= 0 {
        return ffi::SQLITE_CANTOPEN;
    }
    let Ok(path) = (unsafe { CStr::from_ptr(name) }).to_str() else {
        return ffi::SQLITE_CANTOPEN;
    };
    let Ok(normalized) = normalize_path(Path::new(path)) else {
        return ffi::SQLITE_CANTOPEN;
    };
    let bytes = normalized.as_bytes();
    let Ok(output_len) = usize::try_from(output_len) else {
        return ffi::SQLITE_CANTOPEN;
    };
    if bytes
        .len()
        .checked_add(1)
        .is_none_or(|len| len > output_len)
    {
        return ffi::SQLITE_CANTOPEN;
    }
    unsafe {
        ptr::copy_nonoverlapping(bytes.as_ptr(), output.cast::<u8>(), bytes.len());
        *output.add(bytes.len()) = 0;
    }
    ffi::SQLITE_OK
}

pub(super) unsafe extern "C" fn vfs_randomness(
    _vfs: *mut ffi::sqlite3_vfs,
    amount: c_int,
    out: *mut c_char,
) -> c_int {
    if amount <= 0 || out.is_null() {
        return 0;
    }
    let Ok(len) = usize::try_from(amount) else {
        return 0;
    };
    let dest = unsafe { std::slice::from_raw_parts_mut(out.cast::<u8>(), len) };
    if fill_random(dest).is_ok() {
        amount
    } else {
        0
    }
}

pub(super) unsafe extern "C" fn vfs_sleep(
    _vfs: *mut ffi::sqlite3_vfs,
    microseconds: c_int,
) -> c_int {
    let Ok(microseconds_u64) = u64::try_from(microseconds) else {
        return 0;
    };
    std::thread::sleep(Duration::from_micros(microseconds_u64));
    microseconds
}

pub(super) unsafe extern "C" fn vfs_current_time(
    _vfs: *mut ffi::sqlite3_vfs,
    out: *mut f64,
) -> c_int {
    if out.is_null() {
        return ffi::SQLITE_ERROR;
    }
    let unix_seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64());
    unsafe {
        *out = unix_seconds / 86_400.0 + 2_440_587.5;
    }
    ffi::SQLITE_OK
}

pub(super) unsafe extern "C" fn vfs_get_last_error(
    _vfs: *mut ffi::sqlite3_vfs,
    _amount: c_int,
    _out: *mut c_char,
) -> c_int {
    0
}

fn temp_path() -> std::io::Result<PathBuf> {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let id = NEXT_ID
        .fetch_update(AtomicOrdering::Relaxed, AtomicOrdering::Relaxed, |id| {
            id.checked_add(1)
        })
        .map_err(|_| invalid_data("compressed VFS temporary-file id overflow"))?
        + 1;
    Ok(std::env::temp_dir().join(format!(
        "uqa-compressed-sqlite-{}-{id}.tmp",
        std::process::id()
    )))
}
