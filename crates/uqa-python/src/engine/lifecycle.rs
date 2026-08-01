//! Engine creation, file detection, compression, and session creation.

use super::{
    compression_options, database_file_format_name, pyfunction, pymethods, runtime_error, Arc,
    Engine, PathBuf, PyEngine, PyIOError, PyResult,
};

#[pymethods]
impl PyEngine {
    #[new]
    fn new() -> Self {
        Self {
            inner: Arc::new(Engine::new()),
        }
    }

    #[staticmethod]
    fn open(path: PathBuf) -> PyResult<Self> {
        Ok(Self {
            inner: Arc::new(Engine::open(&path).map_err(runtime_error)?),
        })
    }

    /// Create an independent SQL session over the same persistent database.
    /// Transaction state, prepared statements, variables, search path, and
    /// cancellation are isolated while durable data remains shared.
    fn new_session(&self) -> PyResult<Self> {
        Ok(Self {
            inner: Arc::new(self.inner.new_session().map_err(runtime_error)?),
        })
    }

    #[staticmethod]
    fn open_encrypted(path: PathBuf, key: &str) -> PyResult<Self> {
        Ok(Self {
            inner: Arc::new(Engine::open_encrypted(&path, key).map_err(runtime_error)?),
        })
    }

    #[staticmethod]
    #[pyo3(signature = (path, key=None))]
    fn open_auto(path: PathBuf, key: Option<&str>) -> PyResult<Self> {
        Ok(Self {
            inner: Arc::new(Engine::open_auto(&path, key).map_err(runtime_error)?),
        })
    }

    #[staticmethod]
    fn detect_database_file(path: PathBuf) -> PyResult<&'static str> {
        let format = Engine::detect_database_file(&path)
            .map_err(|err| PyIOError::new_err(err.to_string()))?;
        Ok(database_file_format_name(format))
    }

    #[staticmethod]
    #[pyo3(signature = (path, codec="zstd", page_size=None, chunk_pages=None, level=None))]
    fn open_compressed(
        path: PathBuf,
        codec: &str,
        page_size: Option<u32>,
        chunk_pages: Option<u32>,
        level: Option<i32>,
    ) -> PyResult<Self> {
        let compression = compression_options(codec, page_size, chunk_pages, level)?;
        Ok(Self {
            inner: Arc::new(Engine::open_compressed(&path, compression).map_err(runtime_error)?),
        })
    }

    #[staticmethod]
    #[pyo3(signature = (path, key, codec="zstd", page_size=None, chunk_pages=None, level=None))]
    fn open_compressed_encrypted(
        path: PathBuf,
        key: &str,
        codec: &str,
        page_size: Option<u32>,
        chunk_pages: Option<u32>,
        level: Option<i32>,
    ) -> PyResult<Self> {
        let compression = compression_options(codec, page_size, chunk_pages, level)?;
        Ok(Self {
            inner: Arc::new(
                Engine::open_compressed_encrypted(&path, key, compression)
                    .map_err(runtime_error)?,
            ),
        })
    }
}

#[pyfunction]
pub(crate) fn open(path: PathBuf) -> PyResult<PyEngine> {
    PyEngine::open(path)
}

#[pyfunction]
pub(crate) fn open_encrypted(path: PathBuf, key: &str) -> PyResult<PyEngine> {
    PyEngine::open_encrypted(path, key)
}

#[pyfunction]
#[pyo3(signature = (path, key=None))]
pub(crate) fn open_auto(path: PathBuf, key: Option<&str>) -> PyResult<PyEngine> {
    PyEngine::open_auto(path, key)
}

#[pyfunction]
pub(crate) fn detect_database_file(path: PathBuf) -> PyResult<&'static str> {
    PyEngine::detect_database_file(path)
}

#[pyfunction]
#[pyo3(signature = (path, codec="zstd", page_size=None, chunk_pages=None, level=None))]
pub(crate) fn open_compressed(
    path: PathBuf,
    codec: &str,
    page_size: Option<u32>,
    chunk_pages: Option<u32>,
    level: Option<i32>,
) -> PyResult<PyEngine> {
    PyEngine::open_compressed(path, codec, page_size, chunk_pages, level)
}

#[pyfunction]
#[pyo3(signature = (path, key, codec="zstd", page_size=None, chunk_pages=None, level=None))]
pub(crate) fn open_compressed_encrypted(
    path: PathBuf,
    key: &str,
    codec: &str,
    page_size: Option<u32>,
    chunk_pages: Option<u32>,
    level: Option<i32>,
) -> PyResult<PyEngine> {
    PyEngine::open_compressed_encrypted(path, key, codec, page_size, chunk_pages, level)
}
