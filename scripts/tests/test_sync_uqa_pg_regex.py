#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

from __future__ import annotations

import importlib.util
import pathlib
import shutil
import sys
import tempfile
import unittest
from unittest import mock


ROOT = pathlib.Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "sync-uqa-pg-regex.py"
SPEC = importlib.util.spec_from_file_location("sync_uqa_pg_regex", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
SYNC = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = SYNC
SPEC.loader.exec_module(SYNC)


class SyncUQAPGRegexTest(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.crate = pathlib.Path(self.directory.name) / "uqa-pg-regex"
        for name in ("SHA256SUMS", "UPSTREAM.md"):
            target = self.crate / name
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(SYNC.CRATE / name, target)
        shutil.copytree(SYNC.CRATE / SYNC.IMPORT_ROOT, self.crate / SYNC.IMPORT_ROOT)
        self.notice = self.crate / SYNC.NOTICE.relative_to(SYNC.CRATE)
        self.notice.parent.mkdir(parents=True)
        shutil.copyfile(SYNC.NOTICE, self.notice)
        self.patches = [
            mock.patch.object(SYNC, "CRATE", self.crate),
            mock.patch.object(SYNC, "CHECKSUMS", self.crate / "SHA256SUMS"),
            mock.patch.object(SYNC, "UPSTREAM", self.crate / "UPSTREAM.md"),
            mock.patch.object(SYNC, "NOTICE", self.notice),
        ]
        for patch in self.patches:
            patch.start()

    def tearDown(self) -> None:
        for patch in reversed(self.patches):
            patch.stop()
        self.directory.cleanup()

    def test_the_pinned_tree_verifies(self) -> None:
        self.assertEqual(SYNC.check_tree(), len(SYNC.IMPORTED_FILES))

    def test_changed_missing_and_extra_files_are_reported(self) -> None:
        changed = self.crate / SYNC.IMPORT_ROOT / "src/backend/regex/regexec.c"
        changed.write_bytes(changed.read_bytes() + b"\n")
        missing = self.crate / SYNC.IMPORT_ROOT / "src/include/regex/regerrs.h"
        missing.unlink()
        extra = self.crate / SYNC.IMPORT_ROOT / "src/backend/regex/regc_pg_locale.c"
        extra.write_text("/* not imported */\n", encoding="utf-8")
        with self.assertRaises(RuntimeError) as raised:
            SYNC.check_tree()
        message = str(raised.exception)
        self.assertIn("missing: postgres/src/include/regex/regerrs.h", message)
        self.assertIn("extra: postgres/src/backend/regex/regc_pg_locale.c", message)
        self.assertIn("changed: postgres/src/backend/regex/regexec.c", message)

    def test_checksum_list_must_name_exactly_the_pinned_files(self) -> None:
        checksums = self.crate / "SHA256SUMS"
        lines = checksums.read_text(encoding="utf-8").splitlines(keepends=True)
        checksums.write_text("".join(lines[:-1]), encoding="utf-8")
        with self.assertRaisesRegex(RuntimeError, "exactly the pinned PostgreSQL files"):
            SYNC.check_tree()

    def test_malformed_checksum_lines_are_rejected(self) -> None:
        checksums = self.crate / "SHA256SUMS"
        checksums.write_text("not-a-digest  postgres/COPYRIGHT\n", encoding="utf-8")
        with self.assertRaisesRegex(RuntimeError, "not a SHA-256 checksum line"):
            SYNC.check_tree()

    def test_provenance_document_must_match_the_pin(self) -> None:
        upstream = self.crate / "UPSTREAM.md"
        upstream.write_text(upstream.read_text(encoding="utf-8").replace("REL_18_4", "REL_18_3"), encoding="utf-8")
        with self.assertRaisesRegex(RuntimeError, "UPSTREAM.md differs"):
            SYNC.check_tree()

    def test_provenance_names_the_pin_and_every_adaptation(self) -> None:
        text = SYNC.render_upstream(SYNC.checksum_entries())
        self.assertIn(f"- Tag: `{SYNC.POSTGRESQL_TAG}` (PostgreSQL 18.4)", text)
        self.assertIn(f"- Commit: `{SYNC.POSTGRESQL_COMMIT}`", text)
        for relative in SYNC.IMPORTED_FILES:
            self.assertIn(f"`{SYNC.IMPORT_ROOT}/{relative}`", text)
        for adaptation in SYNC.ADAPTATIONS:
            self.assertIn(adaptation, text)

    def test_the_redistribution_notice_reproduces_the_license_files_unchanged(self) -> None:
        notice = SYNC.render_notice(SYNC.checksum_entries())
        self.assertEqual(self.notice.read_bytes(), notice)
        for relative in SYNC.NOTICE_LICENSES:
            self.assertIn((self.crate / SYNC.IMPORT_ROOT / relative).read_bytes(), notice)
        self.assertIn(f"Source commit: {SYNC.POSTGRESQL_COMMIT}".encode(), notice)
        self.assertTrue(notice.isascii())

    def test_a_notice_that_differs_from_the_pin_is_rejected(self) -> None:
        self.notice.write_bytes(self.notice.read_bytes().replace(b"Henry Spencer", b"H. Spencer", 1))
        with self.assertRaisesRegex(RuntimeError, "POSTGRESQL-REGEX-NOTICE.txt differs"):
            SYNC.check_tree()
        self.notice.unlink()
        with self.assertRaisesRegex(RuntimeError, "cannot read"):
            SYNC.check_tree()

    def test_a_source_whose_tag_moved_is_rejected(self) -> None:
        def resolve(_source: pathlib.Path, revision: str) -> str:
            return "0" * 40 if revision.startswith("refs/tags/") else SYNC.POSTGRESQL_COMMIT

        with mock.patch.object(SYNC, "resolve_commit", side_effect=resolve):
            with self.assertRaisesRegex(RuntimeError, f"{SYNC.POSTGRESQL_TAG} is 0+"):
                SYNC.verify_source(pathlib.Path(self.directory.name))


if __name__ == "__main__":
    unittest.main()
