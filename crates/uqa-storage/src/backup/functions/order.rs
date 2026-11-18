//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::cmp::Ordering;

use uqa_core::{
    image_io::{ImageIoError, ImageUtf8},
    memory::BudgetedString,
};

use super::{BackupError, BackupFunctionKind, BackupResult, StorageReadControl};

pub(super) struct Name {
    text: BudgetedString,
    utf8: ImageUtf8,
    ordering: Ordering,
    pub(super) bytes: usize,
    pub(super) length: usize,
    kind: BackupFunctionKind,
}

impl Name {
    pub(super) fn new(
        kind: BackupFunctionKind,
        length: usize,
        control: &StorageReadControl,
    ) -> BackupResult<Self> {
        if length == 0 {
            return Err(BackupError::Invalid("empty registered SQL function name"));
        }
        let mut text = BudgetedString::new(control.memory());
        // A later one-byte advance must never regrow and recopy an arbitrary existing prefix.
        text.reserve(length)?;
        Ok(Self {
            text,
            utf8: ImageUtf8::default(),
            ordering: Ordering::Equal,
            bytes: 0,
            length,
            kind,
        })
    }
}

#[derive(Default)]
pub(super) struct Order {
    previous: Option<(BudgetedString, BackupFunctionKind)>,
    kinds: u8,
}

impl Order {
    pub(super) fn append(&self, name: &mut Name, fragment: &[u8]) -> BackupResult<()> {
        if fragment
            .iter()
            .any(|&byte| byte == 0 || byte.is_ascii_uppercase())
        {
            return Err(BackupError::Invalid(
                "noncanonical registered SQL function name",
            ));
        }
        if let Some((previous, _)) = &self.previous {
            if name.ordering == Ordering::Equal {
                let tail = previous.as_bytes().get(name.bytes..).unwrap_or_default();
                let shared = tail.len().min(fragment.len());
                name.ordering = fragment[..shared].cmp(&tail[..shared]);
                if name.ordering == Ordering::Equal && shared < fragment.len() {
                    name.ordering = Ordering::Greater;
                }
            }
        }
        name.utf8
            .append(fragment, &mut name.text)
            .map_err(image_error)?;
        name.bytes += fragment.len();
        Ok(())
    }

    pub(super) fn finish(&mut self, name: Name) -> BackupResult<()> {
        name.utf8.finish().map_err(image_error)?;
        if name.text.starts_with(char::is_whitespace) || name.text.ends_with(char::is_whitespace) {
            return Err(BackupError::Invalid(
                "noncanonical registered SQL function name",
            ));
        }
        let mut kinds = 0;
        if let Some((previous, kind)) = &self.previous {
            match name.ordering.then_with(|| name.length.cmp(&previous.len())) {
                Ordering::Less => {
                    return Err(BackupError::Invalid("unordered registered SQL function"))
                }
                Ordering::Equal if name.kind <= *kind => {
                    return Err(BackupError::Invalid(
                        "duplicate or unordered registered SQL function",
                    ))
                }
                Ordering::Equal => kinds = self.kinds,
                Ordering::Greater => (),
            }
        }
        kinds |= 1 << (name.kind as u8 - 1);
        if kinds & (kinds >> 3) != 0 {
            return Err(BackupError::Invalid(
                "contradictory registered SQL function declarations",
            ));
        }
        self.kinds = kinds;
        self.previous = Some((name.text, name.kind));
        Ok(())
    }

    #[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
    pub(super) fn current(&self) -> Option<super::BackupFunctionReference<'_>> {
        let (name, kind) = self.previous.as_ref()?;
        Some(super::BackupFunctionReference { name, kind: *kind })
    }
}

fn image_error(error: ImageIoError) -> BackupError {
    match error {
        ImageIoError::Invalid => BackupError::Invalid("registered SQL function name UTF-8"),
        ImageIoError::Memory(error) => error.into(),
        ImageIoError::Cancelled(error) => error.into(),
        ImageIoError::Io(error) => error.into(),
    }
}
