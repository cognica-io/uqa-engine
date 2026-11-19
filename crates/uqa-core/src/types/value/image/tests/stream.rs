//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::{cell::Cell, panic::AssertUnwindSafe, rc::Rc};

fn collect<'a, I: ExactSizeIterator<Item = (&'a str, &'a Value)>>(
    reader: &mut ValueImageReader<'a, '_, I>,
    window: usize,
) -> ValueImageResult<Vec<u8>> {
    let mut output = Vec::new();
    let mut buffer = vec![0; window];
    loop {
        let count = reader.read(&mut buffer)?;
        assert!(count <= window.min(WINDOW));
        if count == 0 {
            return Ok(output);
        }
        output.extend_from_slice(&buffer[..count]);
    }
}

#[test]
fn value_image_reads_resume_at_every_carrier_boundary_without_changing_bytes() {
    let value = Value::List(fixtures());
    let expected = encode(&value);
    for window in [1, 2, 7, 17, WINDOW - 1, WINDOW * 2] {
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let mut reader = ValueImageReader::new(&value, &memory, &token);
        assert_eq!(reader.read(&mut []).unwrap(), 0);
        assert_eq!(collect(&mut reader, window).unwrap(), expected);
        assert_eq!(memory.used(), 0);
        token.cancel();
        assert_eq!(reader.read(&mut [0; 1]).unwrap(), 0);
        assert_eq!(reader.read(&mut []).unwrap(), 0);
    }
}

#[test]
fn value_image_reads_borrow_large_payloads_and_bound_each_output_window() {
    assert!(size_of::<ValueImageReader<'_, '_>>() <= WINDOW);
    let value = Value::List(vec![
        Value::Str("x".repeat(2 << 20)),
        Value::Bytes(vec![0xff; 2 << 20]),
        Value::Datum(DatumValue::new(25, u32::MAX, vec![0x83; 2 << 20])),
    ]);
    let expected = encode(&value);
    let mut actual = vec![0; expected.len()];
    let memory = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let allocation = allocation_counter::measure(|| {
        let mut reader = ValueImageReader::new(&value, &memory, &token);
        let mut position = 0;
        while position != actual.len() {
            let limit = (position + WINDOW * 2).min(actual.len());
            let count = reader.read(&mut actual[position..limit]).unwrap();
            assert!(count > 0 && count <= WINDOW);
            position += count;
        }
        assert_eq!(reader.read(&mut [0; 1]).unwrap(), 0);
    });
    assert_eq!(allocation.bytes_total, 0, "{allocation:?}");
    assert_eq!(actual, expected);
    assert_eq!(memory.used(), 0);
}

#[test]
fn value_image_field_reads_visit_only_the_current_position() {
    let value = Value::Bytes(vec![3; WINDOW * 2]);
    let visits = Cell::new(0);
    let fields = [("a", &value), ("b", &Value::Null)]
        .into_iter()
        .inspect(|_| visits.set(visits.get() + 1));
    let memory = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let mut reader = ValueImageReader::from_fields(fields, &memory, &token);
    assert_eq!(visits.get(), 0);
    let mut header = [0; 17];
    assert_eq!(reader.read(&mut header).unwrap(), 17);
    assert_eq!(visits.get(), 0);
    assert_eq!(&header[..9], b"UQAVAL01\x11");
    assert_eq!(&header[9..], &2_u64.to_le_bytes());
    let mut byte = [0];
    for _ in 0..WINDOW {
        assert_eq!(reader.read(&mut byte).unwrap(), 1);
        assert_eq!(visits.get(), 1);
    }
    token.cancel();
    assert!(matches!(
        reader.read(&mut byte),
        Err(ValueImageError::Cancelled(_))
    ));
    assert!(matches!(
        reader.read(&mut byte),
        Err(ValueImageError::Invalid("failed image reader"))
    ));
    assert_eq!(visits.get(), 1);
}

#[test]
fn value_image_decimal_reads_release_scratch_on_completion_cancel_denial_and_drop() {
    let value = Value::Decimal(DecimalValue::parse(&format!("123.{}", "4".repeat(2048))).unwrap());
    for cancel in [false, true] {
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let mut reader = ValueImageReader::new(&value, &memory, &token);
        assert_eq!(reader.read(&mut [0; 9]).unwrap(), 9);
        assert!(memory.used() >= 2052);
        if cancel {
            token.cancel();
            assert!(matches!(
                reader.read(&mut [0; 1]),
                Err(ValueImageError::Cancelled(_))
            ));
            assert_eq!(memory.used(), 0);
            assert!(matches!(
                reader.read(&mut [0; 1]),
                Err(ValueImageError::Invalid("failed image reader"))
            ));
        }
        drop(reader);
        assert_eq!(memory.used(), 0);
    }
    let memory = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let mut reader = ValueImageReader::new(&value, &memory, &token);
    assert_eq!(reader.read(&mut [0; 8]).unwrap(), 8);
    assert!(matches!(
        reader.read(&mut [0; 1]),
        Err(ValueImageError::Memory(_))
    ));
    assert_eq!(memory.used(), 0);
    assert!(matches!(
        reader.read(&mut [0; 1]),
        Err(ValueImageError::Invalid("failed image reader"))
    ));
    let token = CancellationToken::new();
    token.cancel();
    let mut reader = ValueImageReader::new(&value, &memory, &token);
    let mut output = [0x55; 16];
    assert!(matches!(
        reader.read(&mut output),
        Err(ValueImageError::Cancelled(_))
    ));
    assert_eq!(output, [0x55; 16]);
}

struct Fields<'a> {
    entries: std::array::IntoIter<(&'a str, &'a Value), 2>,
    declared: usize,
    panic_on: Option<usize>,
    calls: usize,
    drops: Rc<Cell<usize>>,
}

impl<'a> Iterator for Fields<'a> {
    type Item = (&'a str, &'a Value);

    fn next(&mut self) -> Option<Self::Item> {
        assert_ne!(self.panic_on, Some(self.calls), "field producer unwind");
        self.calls += 1;
        self.entries.next()
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.declared, Some(self.declared))
    }
}

impl ExactSizeIterator for Fields<'_> {}

impl Drop for Fields<'_> {
    fn drop(&mut self) {
        self.drops.set(self.drops.get() + 1);
    }
}

#[test]
fn value_image_field_errors_and_unwind_permanently_close_the_reader() {
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let value = Value::Decimal(DecimalValue::parse("1.200").unwrap());
    for (names, declared, panic_on, reason) in [
        (["a", "b"], 1, None, "ordered fields"),
        (["a", "b"], 3, None, "field count"),
        (["a", "a"], 2, None, "ordered fields"),
        (["z", "a"], 2, None, "ordered fields"),
        (["a", "b"], 2, Some(0), ""),
        (["a", "b"], 2, Some(1), ""),
    ] {
        let drops = Rc::new(Cell::new(0));
        let fields = Fields {
            entries: [(names[0], &value), (names[1], &Value::Null)].into_iter(),
            declared,
            panic_on,
            calls: 0,
            drops: Rc::clone(&drops),
        };
        let mut reader = ValueImageReader::from_fields(fields, &memory, &token);
        let result = std::panic::catch_unwind(AssertUnwindSafe(|| collect(&mut reader, 1)));
        if panic_on.is_some() {
            assert!(result.is_err());
        } else {
            assert!(
                matches!(result.unwrap(), Err(ValueImageError::Invalid(error)) if error == reason)
            );
        }
        assert_eq!(memory.used(), 0);
        assert_eq!(drops.get(), 1);
        assert!(matches!(
            reader.read(&mut [0; 1]),
            Err(ValueImageError::Invalid("failed image reader"))
        ));
        drop(reader);
        assert_eq!(drops.get(), 1);
    }
}

#[test]
fn value_image_reads_keep_the_nesting_limit_across_mixed_containers() {
    let mut value = Value::Bytes(vec![1, 2]);
    for index in 0..MAX_DEPTH {
        value = match index % 4 {
            0 => Value::List(vec![value]),
            1 => Value::Map(BTreeMap::from([("field".into(), value)])),
            2 => Value::Record(RecordValue::new(vec![("field".into(), value)])),
            _ => Value::Row(
                RowValue::typed(
                    vec![value],
                    vec![RecordFieldType {
                        oid: 25,
                        type_modifier: -1,
                        collation_oid: Some(100),
                    }],
                )
                .unwrap(),
            ),
        };
    }
    let memory = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let mut reader = ValueImageReader::new(&value, &memory, &token);
    assert_eq!(collect(&mut reader, 1).unwrap(), encode(&value));
    let value = Value::Map(BTreeMap::from([("excess".into(), value)]));
    let mut reader = ValueImageReader::new(&value, &memory, &token);
    assert!(matches!(
        collect(&mut reader, 1),
        Err(ValueImageError::Invalid("nesting depth"))
    ));
    assert!(matches!(
        reader.read(&mut [0; 1]),
        Err(ValueImageError::Invalid("failed image reader"))
    ));
    assert_eq!(memory.used(), 0);
}

#[test]
fn value_image_writer_preserves_interruption_zero_write_and_invalid_count_errors() {
    struct Output {
        mode: usize,
        calls: usize,
        token: CancellationToken,
    }
    impl Write for Output {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.calls += 1;
            match self.mode {
                0 => Ok(0),
                1 => Ok(bytes.len() + 1),
                2 => {
                    if self.calls == 3 {
                        self.token.cancel();
                    }
                    Err(std::io::ErrorKind::Interrupted.into())
                }
                _ => {
                    self.token.cancel();
                    Ok(bytes.len())
                }
            }
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let memory = MemoryBudget::new(0);
    for mode in 0..4 {
        let token = CancellationToken::new();
        let mut output = Output {
            mode,
            calls: 0,
            token: token.clone(),
        };
        let result = write_value_image(&Value::Null, &mut output, &memory, &token);
        match mode {
            0 => assert!(
                matches!(result, Err(ValueImageError::Io(error)) if error.kind() == std::io::ErrorKind::WriteZero)
            ),
            1 => assert!(matches!(
                result,
                Err(ValueImageError::Invalid("writer byte count"))
            )),
            _ => assert!(matches!(result, Err(ValueImageError::Cancelled(_)))),
        }
        assert_eq!(output.calls, if mode == 2 { 3 } else { 1 });
        assert_eq!(memory.used(), 0);
    }
}
