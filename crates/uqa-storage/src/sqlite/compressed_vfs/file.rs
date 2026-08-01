//! Uniform byte-file abstraction for compressed main files and plain auxiliaries.

use super::{
    c_int, ffi, fs, invalid_data, usize_to_u64, ContainerFile, OpenOptions, OpenOptionsEntry, Path,
    PathBuf, PlainFile, Read, Seek, SeekFrom, VfsFile, Write,
};

impl VfsFile {
    pub(super) fn open(
        path: PathBuf,
        options: OpenOptionsEntry,
        flags: c_int,
        read_only: bool,
    ) -> std::io::Result<Self> {
        if options.key.is_none() && should_store_plain(flags, &path) {
            PlainFile::open(path, read_only, flags & ffi::SQLITE_OPEN_CREATE != 0).map(Self::Plain)
        } else {
            ContainerFile::open(path, options).map(Self::Compressed)
        }
    }

    pub(super) fn path(&self) -> &Path {
        match self {
            Self::Compressed(file) => &file.path,
            Self::Plain(file) => &file.path,
        }
    }

    pub(super) fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::Compressed(file) => file.flush(),
            Self::Plain(file) => file.flush(),
        }
    }

    pub(super) fn read_at(&mut self, offset: usize, dest: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Compressed(file) => file.read_at(offset, dest),
            Self::Plain(file) => file.read_at(offset, dest),
        }
    }

    pub(super) fn write_at(&mut self, offset: usize, source: &[u8]) -> std::io::Result<()> {
        match self {
            Self::Compressed(file) => file.write_at(offset, source),
            Self::Plain(file) => file.write_at(offset, source),
        }
    }

    pub(super) fn truncate_to(&mut self, size: usize) -> std::io::Result<()> {
        match self {
            Self::Compressed(file) => file.truncate_to(size),
            Self::Plain(file) => file.truncate_to(size),
        }
    }

    pub(super) fn size(&self) -> std::io::Result<usize> {
        match self {
            Self::Compressed(file) => Ok(file.logical_len),
            Self::Plain(file) => file.size(),
        }
    }
}

impl PlainFile {
    pub(super) fn open(path: PathBuf, read_only: bool, create: bool) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut options = OpenOptions::new();
        options.read(true);
        if !read_only {
            options.write(true);
            if create {
                options.create(true);
            }
        }
        let file = options.open(&path)?;
        Ok(Self { path, file })
    }

    pub(super) fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()?;
        self.file.sync_all()
    }

    pub(super) fn read_at(&mut self, offset: usize, dest: &mut [u8]) -> std::io::Result<usize> {
        let offset = usize_to_u64(offset, "plain-file read offset")?;
        self.file.seek(SeekFrom::Start(offset))?;
        let mut read = 0;
        while read < dest.len() {
            let n = self.file.read(&mut dest[read..])?;
            if n == 0 {
                break;
            }
            read += n;
        }
        dest[read..].fill(0);
        Ok(read)
    }

    pub(super) fn write_at(&mut self, offset: usize, source: &[u8]) -> std::io::Result<()> {
        let offset = usize_to_u64(offset, "plain-file write offset")?;
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.write_all(source)
    }

    pub(super) fn truncate_to(&mut self, size: usize) -> std::io::Result<()> {
        self.file
            .set_len(usize_to_u64(size, "plain-file truncate size")?)
    }

    pub(super) fn size(&self) -> std::io::Result<usize> {
        usize::try_from(self.file.metadata()?.len())
            .map_err(|_| invalid_data("plain file too large"))
    }
}

fn should_store_plain(flags: c_int, path: &Path) -> bool {
    let auxiliary_flags = ffi::SQLITE_OPEN_MAIN_JOURNAL
        | ffi::SQLITE_OPEN_TEMP_JOURNAL
        | ffi::SQLITE_OPEN_SUBJOURNAL
        | ffi::SQLITE_OPEN_SUPER_JOURNAL
        | ffi::SQLITE_OPEN_WAL;
    if flags & auxiliary_flags != 0 {
        return true;
    }
    let normalized = path.to_string_lossy();
    normalized.ends_with("-journal") || normalized.ends_with("-wal") || normalized.ends_with("-shm")
}
