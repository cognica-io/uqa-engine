//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `SQLite` file-method callbacks and locking protocol.

use super::{
    c_int, c_void, ffi, ptr, CompressedSQLiteFile, FileHandle, DEFAULT_PAGE_SIZE,
    SQLITE_LOCK_EXCLUSIVE, SQLITE_LOCK_NONE,
};

unsafe fn file_from_sqlite<'a>(file: *mut ffi::sqlite3_file) -> Option<&'a mut FileHandle> {
    let compressed = file.cast::<CompressedSQLiteFile>();
    let handle = unsafe { (*compressed).handle };
    if handle.is_null() {
        None
    } else {
        Some(unsafe { &mut *handle })
    }
}

pub(super) static IO_METHODS: ffi::sqlite3_io_methods = ffi::sqlite3_io_methods {
    iVersion: 1,
    xClose: Some(file_close),
    xRead: Some(file_read),
    xWrite: Some(file_write),
    xTruncate: Some(file_truncate),
    xSync: Some(file_sync),
    xFileSize: Some(file_size),
    xLock: Some(file_lock),
    xUnlock: Some(file_unlock),
    xCheckReservedLock: Some(file_check_reserved_lock),
    xFileControl: Some(file_control),
    xSectorSize: Some(file_sector_size),
    xDeviceCharacteristics: Some(file_device_characteristics),
    xShmMap: None,
    xShmLock: None,
    xShmBarrier: None,
    xShmUnmap: None,
    xFetch: None,
    xUnfetch: None,
};

unsafe extern "C" fn file_close(file: *mut ffi::sqlite3_file) -> c_int {
    let compressed = file.cast::<CompressedSQLiteFile>();
    let handle = unsafe { (*compressed).handle };
    if handle.is_null() {
        return ffi::SQLITE_OK;
    }
    unsafe {
        (*compressed).handle = ptr::null_mut();
    }
    let mut handle = unsafe { Box::from_raw(handle) };
    let flush = handle.file.flush();
    let unlock = handle.locks.unlock(SQLITE_LOCK_NONE);
    let delete_path = handle
        .delete_on_close
        .then(|| handle.file.path().to_owned());
    drop(handle);
    let delete = delete_path.map_or(Ok(false), |path| {
        super::vfs_callbacks::remove_file_and_locks(&path)
    });
    unsafe {
        (*compressed).base.pMethods = ptr::null();
    }
    if flush.is_ok() && unlock.is_ok() && delete.is_ok() {
        ffi::SQLITE_OK
    } else {
        ffi::SQLITE_IOERR_CLOSE
    }
}

unsafe extern "C" fn file_read(
    file: *mut ffi::sqlite3_file,
    out: *mut c_void,
    amount: c_int,
    offset: ffi::sqlite3_int64,
) -> c_int {
    let Some(handle) = (unsafe { file_from_sqlite(file) }) else {
        return ffi::SQLITE_IOERR_READ;
    };
    if out.is_null() || amount < 0 || offset < 0 {
        return ffi::SQLITE_IOERR_READ;
    }
    let (Ok(amount), Ok(offset)) = (usize::try_from(amount), usize::try_from(offset)) else {
        return ffi::SQLITE_IOERR_READ;
    };
    let dest = unsafe { std::slice::from_raw_parts_mut(out.cast::<u8>(), amount) };
    match handle.file.read_at(offset, dest) {
        Ok(copy_len) if copy_len == amount => ffi::SQLITE_OK,
        Ok(_) => ffi::SQLITE_IOERR_SHORT_READ,
        Err(_) => ffi::SQLITE_IOERR_READ,
    }
}

unsafe extern "C" fn file_write(
    file: *mut ffi::sqlite3_file,
    input: *const c_void,
    amount: c_int,
    offset: ffi::sqlite3_int64,
) -> c_int {
    let Some(handle) = (unsafe { file_from_sqlite(file) }) else {
        return ffi::SQLITE_IOERR_WRITE;
    };
    if handle.read_only {
        return ffi::SQLITE_READONLY;
    }
    if input.is_null() || amount < 0 || offset < 0 {
        return ffi::SQLITE_IOERR_WRITE;
    }
    let (Ok(amount), Ok(offset)) = (usize::try_from(amount), usize::try_from(offset)) else {
        return ffi::SQLITE_IOERR_WRITE;
    };
    let source = unsafe { std::slice::from_raw_parts(input.cast::<u8>(), amount) };
    match handle.file.write_at(offset, source) {
        Ok(()) => ffi::SQLITE_OK,
        Err(_) => ffi::SQLITE_IOERR_WRITE,
    }
}

unsafe extern "C" fn file_truncate(
    file: *mut ffi::sqlite3_file,
    size: ffi::sqlite3_int64,
) -> c_int {
    let Some(handle) = (unsafe { file_from_sqlite(file) }) else {
        return ffi::SQLITE_IOERR_TRUNCATE;
    };
    if handle.read_only || size < 0 {
        return ffi::SQLITE_IOERR_TRUNCATE;
    }
    let Ok(size) = usize::try_from(size) else {
        return ffi::SQLITE_IOERR_TRUNCATE;
    };
    match handle.file.truncate_to(size) {
        Ok(()) => ffi::SQLITE_OK,
        Err(_) => ffi::SQLITE_IOERR_TRUNCATE,
    }
}

unsafe extern "C" fn file_sync(file: *mut ffi::sqlite3_file, _flags: c_int) -> c_int {
    let Some(handle) = (unsafe { file_from_sqlite(file) }) else {
        return ffi::SQLITE_IOERR_FSYNC;
    };
    match handle.file.flush() {
        Ok(()) => ffi::SQLITE_OK,
        Err(_) => ffi::SQLITE_IOERR_FSYNC,
    }
}

unsafe extern "C" fn file_size(
    file: *mut ffi::sqlite3_file,
    size_out: *mut ffi::sqlite3_int64,
) -> c_int {
    if size_out.is_null() {
        return ffi::SQLITE_IOERR_FSTAT;
    }
    let Some(handle) = (unsafe { file_from_sqlite(file) }) else {
        return ffi::SQLITE_IOERR_FSTAT;
    };
    let Ok(size) = handle.file.size() else {
        return ffi::SQLITE_IOERR_FSTAT;
    };
    let Ok(size) = ffi::sqlite3_int64::try_from(size) else {
        return ffi::SQLITE_IOERR_FSTAT;
    };
    unsafe {
        *size_out = size;
    }
    ffi::SQLITE_OK
}

unsafe extern "C" fn file_lock(file: *mut ffi::sqlite3_file, lock: c_int) -> c_int {
    let Some(handle) = (unsafe { file_from_sqlite(file) }) else {
        return ffi::SQLITE_IOERR_LOCK;
    };
    let previous_lock = handle.locks.level();
    match handle.locks.lock(lock) {
        Ok(false) => return ffi::SQLITE_BUSY,
        Err(_) => return ffi::SQLITE_IOERR_LOCK,
        Ok(true) => {}
    }
    if previous_lock == SQLITE_LOCK_NONE && handle.file.refresh_committed_state().is_err() {
        let _ = handle.locks.unlock(SQLITE_LOCK_NONE);
        return ffi::SQLITE_IOERR_LOCK;
    }
    ffi::SQLITE_OK
}

unsafe extern "C" fn file_unlock(file: *mut ffi::sqlite3_file, lock: c_int) -> c_int {
    let Some(handle) = (unsafe { file_from_sqlite(file) }) else {
        return ffi::SQLITE_IOERR_UNLOCK;
    };
    // SQLite may truncate the main database after its final xSync during VACUUM. Publish the remaining mutation before releasing the exclusive lock.
    let releasing_exclusive =
        handle.locks.level() == SQLITE_LOCK_EXCLUSIVE && lock < SQLITE_LOCK_EXCLUSIVE;
    if (lock == SQLITE_LOCK_NONE || releasing_exclusive) && handle.file.flush().is_err() {
        return ffi::SQLITE_IOERR_UNLOCK;
    }
    match handle.locks.unlock(lock) {
        Ok(()) => ffi::SQLITE_OK,
        Err(_) => ffi::SQLITE_IOERR_UNLOCK,
    }
}

unsafe extern "C" fn file_check_reserved_lock(
    file: *mut ffi::sqlite3_file,
    out: *mut c_int,
) -> c_int {
    if out.is_null() {
        return ffi::SQLITE_IOERR_CHECKRESERVEDLOCK;
    }
    let Some(handle) = (unsafe { file_from_sqlite(file) }) else {
        return ffi::SQLITE_IOERR_CHECKRESERVEDLOCK;
    };
    match handle.locks.check_reserved() {
        Ok(reserved) => {
            unsafe {
                *out = i32::from(reserved);
            }
            ffi::SQLITE_OK
        }
        Err(_) => ffi::SQLITE_IOERR_CHECKRESERVEDLOCK,
    }
}

unsafe extern "C" fn file_control(
    _file: *mut ffi::sqlite3_file,
    _op: c_int,
    _arg: *mut c_void,
) -> c_int {
    ffi::SQLITE_NOTFOUND
}

unsafe extern "C" fn file_sector_size(_file: *mut ffi::sqlite3_file) -> c_int {
    DEFAULT_PAGE_SIZE as c_int
}

unsafe extern "C" fn file_device_characteristics(_file: *mut ffi::sqlite3_file) -> c_int {
    0
}
