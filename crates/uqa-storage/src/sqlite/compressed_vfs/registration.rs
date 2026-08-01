//! Per-database options registry and process-wide `SQLite` VFS registration.

use super::{
    c_char, c_int, ffi, invalid_data, ptr, vfs_access, vfs_current_time, vfs_delete,
    vfs_full_pathname, vfs_get_last_error, vfs_open, vfs_randomness, vfs_sleep, BTreeMap,
    Component, CompressedSQLiteFile, Mutex, OpenOptionsEntry, Path, PathBuf,
    SQLiteCompressionOptions, REGISTRY, VFS_NAME_C, VFS_REGISTERED,
};

pub fn register_database(
    path: &Path,
    compression: SQLiteCompressionOptions,
    key: Option<&str>,
) -> Result<(), String> {
    let compression = compression.validate()?;
    ensure_registered().map_err(|code| format!("sqlite3_vfs_register failed with code {code}"))?;
    let entry = OpenOptionsEntry {
        compression,
        key: key.map(str::to_string),
    };
    let mut registry = registry().lock().map_err(|_| "vfs registry poisoned")?;
    let normalized = normalize_path(path).map_err(|error| error.to_string())?;
    registry.insert(normalized, entry);
    Ok(())
}

fn ensure_registered() -> std::result::Result<(), c_int> {
    VFS_REGISTERED
        .get_or_init(|| {
            let vfs = Box::new(ffi::sqlite3_vfs {
                iVersion: 1,
                szOsFile: std::mem::size_of::<CompressedSQLiteFile>() as c_int,
                mxPathname: 4096,
                pNext: ptr::null_mut(),
                zName: VFS_NAME_C.as_ptr().cast::<c_char>(),
                pAppData: ptr::null_mut(),
                xOpen: Some(vfs_open),
                xDelete: Some(vfs_delete),
                xAccess: Some(vfs_access),
                xFullPathname: Some(vfs_full_pathname),
                xDlOpen: None,
                xDlError: None,
                xDlSym: None,
                xDlClose: None,
                xRandomness: Some(vfs_randomness),
                xSleep: Some(vfs_sleep),
                xCurrentTime: Some(vfs_current_time),
                xGetLastError: Some(vfs_get_last_error),
                xCurrentTimeInt64: None,
                xSetSystemCall: None,
                xGetSystemCall: None,
                xNextSystemCall: None,
            });
            let leaked = Box::leak(vfs);
            // SAFETY: `leaked` points to a process-lifetime sqlite3_vfs
            // value whose callback function pointers and name also have
            // process lifetime.
            let rc = unsafe { ffi::sqlite3_vfs_register(leaked, 0) };
            if rc == ffi::SQLITE_OK {
                Ok(())
            } else {
                Err(rc)
            }
        })
        .to_owned()
}

fn registry() -> &'static Mutex<BTreeMap<String, OpenOptionsEntry>> {
    REGISTRY.get_or_init(|| Mutex::new(BTreeMap::new()))
}

pub(super) fn normalize_path(path: &Path) -> std::io::Result<String> {
    let full = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut out = PathBuf::new();
    for component in full.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    Ok(out.to_string_lossy().into_owned())
}

pub(super) fn options_for_path(path: &Path) -> std::io::Result<OpenOptionsEntry> {
    let normalized = normalize_path(path)?;
    let registry = registry()
        .lock()
        .map_err(|_| invalid_data("vfs registry poisoned"))?;
    if let Some(options) = registry.get(&normalized) {
        return Ok(options.clone());
    }
    for suffix in ["-journal", "-wal", "-shm"] {
        if let Some(base) = normalized.strip_suffix(suffix) {
            if let Some(options) = registry.get(base) {
                return Ok(options.clone());
            }
        }
    }
    Ok(OpenOptionsEntry {
        compression: SQLiteCompressionOptions::default(),
        key: None,
    })
}
