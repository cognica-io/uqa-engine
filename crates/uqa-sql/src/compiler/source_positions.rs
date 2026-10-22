//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Convert parser byte locations against the original complete SQL message.

use std::{cell::RefCell, sync::Arc};

#[derive(Debug)]
pub(super) struct SourcePositions {
    length: usize,
    excess_bytes: Vec<(usize, usize)>,
}

impl SourcePositions {
    pub(super) fn new(sql: &str) -> Self {
        let mut excess = 0;
        let excess_bytes = sql
            .char_indices()
            .filter_map(|(offset, character)| {
                let width = character.len_utf8();
                if width == 1 {
                    return None;
                }
                excess += width - 1;
                Some((offset + width, excess))
            })
            .collect();
        Self {
            length: sql.len(),
            excess_bytes,
        }
    }

    fn position(&self, location: i32) -> Option<u32> {
        let offset = usize::try_from(location).ok()?;
        if offset > self.length {
            return None;
        }
        let before = self.excess_bytes.partition_point(|(end, _)| *end <= offset);
        let excess = before
            .checked_sub(1)
            .map_or(0, |index| self.excess_bytes[index].1);
        u32::try_from(offset.checked_sub(excess)?.checked_add(1)?).ok()
    }
}

thread_local! {
    static SOURCE: RefCell<Option<Arc<SourcePositions>>> = const { RefCell::new(None) };
}

pub(super) fn with_source<T>(source: Arc<SourcePositions>, compile: impl FnOnce() -> T) -> T {
    struct Restore(Option<Arc<SourcePositions>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            SOURCE.with(|source| {
                source.replace(self.0.take());
            });
        }
    }
    let _restore = Restore(SOURCE.with(|current| current.replace(Some(source))));
    compile()
}

pub(super) fn position(location: i32) -> Option<u32> {
    SOURCE.with(|source| {
        source
            .borrow()
            .as_ref()
            .and_then(|source| source.position(location))
    })
}

#[cfg(test)]
mod tests {
    use super::{position, with_source, SourcePositions};
    use std::sync::Arc;

    #[test]
    fn parser_byte_offsets_use_characters_in_the_complete_message() {
        let sql = "SELECT '🙂한글'; SELECT t.x";
        let source = SourcePositions::new(sql);
        for (character_index, (byte_index, _)) in sql.char_indices().enumerate() {
            assert_eq!(
                source.position(i32::try_from(byte_index).unwrap()),
                Some(u32::try_from(character_index + 1).unwrap())
            );
        }
        assert_eq!(source.position(-1), None);
        assert_eq!(source.position(i32::try_from(sql.len() + 1).unwrap()), None);
        assert_eq!(
            source.position(i32::try_from(sql.len()).unwrap()),
            Some(u32::try_from(sql.chars().count() + 1).unwrap())
        );
    }

    #[test]
    fn nested_compilation_restores_the_callers_source_after_unwinding() {
        assert_eq!(position(4), None);
        with_source(Arc::new(SourcePositions::new("한 a")), || {
            assert_eq!(position(4), Some(3));
            let result = std::panic::catch_unwind(|| {
                with_source(Arc::new(SourcePositions::new("abcd")), || {
                    assert_eq!(position(4), Some(5));
                    panic!("nested compilation failure");
                });
            });
            assert!(result.is_err());
            assert_eq!(position(4), Some(3));
        });
        assert_eq!(position(4), None);
    }
}
