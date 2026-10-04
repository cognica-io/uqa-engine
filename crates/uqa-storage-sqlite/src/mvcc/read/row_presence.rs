//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Whether rows that every write of a table asks about exist, read at one committed snapshot.

use parking_lot::Mutex;

const ROWS: usize = 64;
const KEY_BYTES: usize = 256;

/// The presence of committed definition rows, such as the marker of a B-tree index, read through one snapshot. Each written row asks whether its table's indexes are built, and a committed record never changes at a snapshot, so each answer is read once for as long as a session keeps the snapshot. A row the reading transaction changed is private to it and is not held here.
#[derive(Default)]
pub(crate) struct RowPresence {
    rows: Mutex<Vec<(Box<[u8]>, bool)>>,
}

impl RowPresence {
    pub(crate) fn get(&self, key: &[u8]) -> Option<bool> {
        self.rows
            .lock()
            .iter()
            .find(|(known, _)| **known == *key)
            .map(|(_, live)| *live)
    }

    pub(crate) fn remember(&self, key: &[u8], live: bool) {
        if key.len() > KEY_BYTES {
            return;
        }
        let mut rows = self.rows.lock();
        if rows.iter().any(|(known, _)| **known == *key) {
            return;
        }
        if rows.len() == ROWS {
            rows.remove(0);
        }
        rows.push((key.into(), live));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_are_kept_by_key_up_to_a_bound() {
        let presence = RowPresence::default();
        assert_eq!(presence.get(b"marker"), None);
        presence.remember(b"marker", true);
        presence.remember(b"missing", false);
        // The first answer read for a key is the committed one and stays.
        presence.remember(b"marker", false);
        assert_eq!(presence.get(b"marker"), Some(true));
        assert_eq!(presence.get(b"missing"), Some(false));
        for row in 0..ROWS as u64 {
            presence.remember(&row.to_be_bytes(), row % 2 == 0);
        }
        // The oldest keys made room.
        assert_eq!(presence.get(b"marker"), None);
        assert_eq!(presence.get(&63_u64.to_be_bytes()), Some(false));
        assert_eq!(presence.rows.lock().len(), ROWS);
        presence.remember(&[7; KEY_BYTES + 1], true);
        assert_eq!(presence.get(&[7; KEY_BYTES + 1]), None);
    }
}
