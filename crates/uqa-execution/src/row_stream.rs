//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Owned row production with an explicit, fallible cleanup boundary.

use uqa_sql::SQLError;

/// A row producer whose resources require fallible cleanup. A successful
/// construction transfers its resources to [`SQLRowStream`]. `close` must
/// release them even when it reports an error; it is invoked at most once.
pub trait SQLRowSource<T>: Send {
    fn next_row(&mut self) -> Result<Option<T>, SQLError>;
    fn close(&mut self) -> Result<(), SQLError>;
}

/// A fused row stream that closes its producer on EOF, failure or explicit
/// early termination. A cleanup failure is a query error, never EOF. Dropping
/// an abandoned stream is best-effort cleanup; execution must call `close`
/// while it can still report an error to its consumer.
pub struct SQLRowStream<'a, T> {
    source: Option<Box<dyn SQLRowSource<T> + 'a>>,
}

impl<'a, T: 'a> SQLRowStream<'a, T> {
    /// Adapt an ordinary iterator with infallible resource release. Sources
    /// with fallible cleanup must use `from_source` instead.
    pub fn new(rows: impl Iterator<Item = Result<T, SQLError>> + Send + 'a) -> Self {
        Self::from_source(IteratorSource(rows))
    }

    pub fn from_source(source: impl SQLRowSource<T> + 'a) -> Self {
        Self {
            source: Some(Box::new(source)),
        }
    }

    /// Transform rows without losing the producer's cleanup contract.
    pub fn map_rows<U: 'a>(
        self,
        map: impl FnMut(T) -> Result<U, SQLError> + Send + 'a,
    ) -> SQLRowStream<'a, U> {
        SQLRowStream::from_source(MappedRows { rows: self, map })
    }
}

impl<T> SQLRowStream<'_, T> {
    /// Consume the producer before invoking cleanup, including when it fails.
    /// Repeated close calls succeed without invoking the producer again.
    pub fn close(&mut self) -> Result<(), SQLError> {
        self.source
            .take()
            .map_or(Ok(()), |mut source| source.close())
    }

    pub(crate) fn fail(&mut self, error: SQLError) -> SQLError {
        match self.close() {
            Ok(()) => error,
            Err(cleanup) => error.with_cleanup(cleanup, "close row stream"),
        }
    }
}

impl<T> Iterator for SQLRowStream<'_, T> {
    type Item = Result<T, SQLError>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.source.as_mut()?.next_row() {
            Ok(Some(row)) => Some(Ok(row)),
            Ok(None) => self.close().err().map(Err),
            Err(error) => Some(Err(self.fail(error))),
        }
    }
}

impl<T> std::iter::FusedIterator for SQLRowStream<'_, T> {}

impl<T> Drop for SQLRowStream<'_, T> {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

struct IteratorSource<I>(I);

impl<T, I: Iterator<Item = Result<T, SQLError>> + Send> SQLRowSource<T> for IteratorSource<I> {
    fn next_row(&mut self) -> Result<Option<T>, SQLError> {
        self.0.next().transpose()
    }

    fn close(&mut self) -> Result<(), SQLError> {
        Ok(())
    }
}

struct MappedRows<'a, T, F> {
    rows: SQLRowStream<'a, T>,
    map: F,
}

impl<T, U, F: FnMut(T) -> Result<U, SQLError> + Send> SQLRowSource<U> for MappedRows<'_, T, F> {
    fn next_row(&mut self) -> Result<Option<U>, SQLError> {
        self.rows.next().transpose()?.map(&mut self.map).transpose()
    }

    fn close(&mut self) -> Result<(), SQLError> {
        self.rows.close()
    }
}

#[cfg(test)]
mod tests;
