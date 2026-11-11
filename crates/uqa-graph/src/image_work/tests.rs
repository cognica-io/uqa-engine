//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::io::Read;

pub(crate) struct Reader<'a> {
    source: &'a [u8],
    pub bytes: usize,
    pub calls: usize,
    pub eof: bool,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(source: &'a [u8]) -> Self {
        Self {
            source,
            bytes: 0,
            calls: 0,
            eof: false,
        }
    }
}

impl Read for Reader<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        assert!(!self.eof, "completed input was replayed");
        assert!(!bytes.is_empty());
        self.calls += 1;
        let count = self.source.len().min(bytes.len());
        bytes[..count].copy_from_slice(&self.source[..count]);
        self.source = &self.source[count..];
        self.bytes += count;
        self.eof = count == 0;
        Ok(count)
    }
}
