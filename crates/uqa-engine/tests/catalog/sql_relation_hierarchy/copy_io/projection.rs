//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded scalar expansion through public COPY output.

use super::Engine;

#[test]
fn copy_scalar_projection_does_not_retain_complete_expanded_batch() {
    assert_bounded_projection(
        "COPY (SELECT repeat(CASE WHEN n>0 THEN 'x' ELSE 'y' END,131072)
         FROM generate_series(1,128) AS s(n)) TO STDOUT",
        false,
    );
}

#[test]
fn copy_ordered_projection_does_not_retain_complete_expanded_batch() {
    assert_bounded_projection(
        "COPY (SELECT lpad(n::text,3,'0') ||
                      repeat(CASE WHEN n>0 THEN 'x' ELSE 'y' END,32765)
         FROM generate_series(128,1,-1) AS s(n) ORDER BY 1) TO STDOUT",
        true,
    );
}

#[test]
fn copy_window_projection_does_not_retain_complete_expanded_batch() {
    assert_bounded_projection(
        "COPY (SELECT repeat(CASE WHEN row_number() OVER ()>0 THEN 'x' ELSE 'y' END,131072)
         FROM generate_series(1,128) AS s(n)) TO STDOUT",
        false,
    );
}

fn assert_bounded_projection(sql: &str, ordered: bool) {
    struct Output {
        column: usize,
        rows: usize,
        ordered: bool,
        width: usize,
    }
    impl std::io::Write for Output {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            for byte in bytes {
                if *byte == b'\n' {
                    assert_eq!(self.column, self.width);
                    self.column = 0;
                    self.rows += 1;
                } else {
                    let expected = if self.ordered && self.column < 3 {
                        let divisor = [100, 10, 1][self.column];
                        b'0' + u8::try_from(((self.rows + 1) / divisor) % 10).unwrap()
                    } else {
                        b'x'
                    };
                    assert_eq!(*byte, expected);
                    self.column += 1;
                }
            }
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let (rows, width, work_mem, ceiling) = if ordered {
        // Sixteen merge inputs may each retain the 32KiB value and its sort key. Allow another 1MiB for bounded input/output batches and codec scratch; the complete 4MiB projection cannot fit.
        (128, 32_768, 64 * 1024, 2 * 1024 * 1024)
    } else {
        (128, 131_072, 512 * 1024, 8 * 1024 * 1024)
    };
    let engine = Engine::new();
    engine.set_query_memory_limit(Some(work_mem));
    let mut output = Output {
        column: 0,
        rows: 0,
        ordered,
        width,
    };
    let allocated = allocation_counter::measure(|| {
        assert_eq!(
            engine.copy_to(sql, &mut output).unwrap(),
            u64::try_from(rows).unwrap()
        );
    });
    // Each complete expanded batch exceeds its fixed public-path allowance.
    assert!(allocated.bytes_max < ceiling, "{allocated:?}");
    assert_eq!((output.column, output.rows), (0, rows));
}
