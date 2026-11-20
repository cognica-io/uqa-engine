#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

from concurrent.futures import ThreadPoolExecutor
import subprocess
import sys
from textwrap import dedent

import pytest

import uqa


@pytest.mark.parametrize("persistent", [False, True], ids=["memory", "sqlite-native"])
@pytest.mark.parametrize("protected", [False, True], ids=["plain", "encrypted"])
def test_incremental_after_removing_parent_and_reopening_repository(tmp_path, persistent, protected):
    credentials = {"key": bytes(range(32)), "protection_domain": bytes(range(16))} if protected else {}
    source = uqa.open(tmp_path / "source.db") if persistent else uqa.Engine()
    repository_path = tmp_path / "backups"
    repository = uqa.BackupRepository.create(repository_path, **credentials)
    assert len(repository.id) == 16
    assert repository.protection_domain == credentials.get("protection_domain")
    source.sql("CREATE TABLE notes(id integer PRIMARY KEY, body text)")
    source.sql("INSERT INTO notes VALUES(1, 'old'), (2, 'delete')")
    full = source.begin_backup(repository).finish()
    assert isinstance(full, bytes) and len(full) == 16
    assert repository.parent(full) is None
    source.sql("UPDATE notes SET body='changed' WHERE id=1")
    source.sql("DELETE FROM notes WHERE id=2")
    source.sql("INSERT INTO notes VALUES(3, 'added')")
    options = uqa.BackupOptions(maximum_encoded_bytes=16 << 20, maximum_workspace_bytes=16 << 20, memory_bytes=32 << 20)
    capture = source.begin_backup(repository, parent=full, options=options)
    assert capture.report is None
    repository.close()
    repository.close()
    with pytest.raises(RuntimeError, match="closed"):
        repository.list()
    source.sql("INSERT INTO notes VALUES(4, 'after the cut')")
    source.close()
    for _ in range(100_000):
        if capture.advance(5, 8192) == "prepared":
            break
    assert capture.phase == "prepared"
    assert capture.progress["parent_units"] > 0
    assert capture.progress["reused_units"] > 0
    assert capture.report["units"] == capture.progress["reused_units"] + capture.progress["written_units"]
    assert len(capture.report["image_digest"]) == 32
    child = capture.publish()
    assert child == capture.id == capture.finish() == capture.cancel()
    assert child != full
    capture.close()
    capture.close()
    repository = uqa.BackupRepository.open(repository_path, **credentials)
    assert set(repository.list()) == {full, child}
    repository.remove(full)
    assert list(repository.list()) == [child]
    assert repository.parent(child) == full
    repository.close()

    if protected:
        with pytest.raises(RuntimeError):
            uqa.BackupRepository.open(repository_path, **dict(credentials, key=b"X" * 32))
    repository = uqa.BackupRepository.open(repository_path, **credentials)
    repository.verify(child, maximum_workspace_bytes=16 << 20)
    with pytest.raises(RuntimeError):
        repository.verify(full)
    authority = uqa.Engine()
    directory = tmp_path / "restored"
    target_options = {"key": "new SQLite key", "codec": "zstd"} if protected else {}
    destination = uqa.RestoreDestination.sqlite(directory, **target_options) if persistent else uqa.RestoreDestination.memory()
    restore_options = uqa.RestoreOptions(maximum_workspace_bytes=16 << 20, maximum_value_bytes=4 << 20, retained_bytes=32 << 20, memory_bytes=32 << 20)
    job = authority.begin_restore(repository, child, destination=destination, options=restore_options)
    assert job.backup_id == child
    with ThreadPoolExecutor(max_workers=1) as executor:
        assert executor.submit(job.advance, 1024).result() in {"reading", "validating"}
    for _ in range(100_000):
        if job.advance(8192) == "prepared":
            break
    assert job.phase == "prepared"
    assert not directory.exists()
    if persistent:
        restore_id = job.id
        assert isinstance(restore_id, bytes) and len(restore_id) == 16
        job.close()
        job.close()
        job = authority.resume_restore(repository, restore_id, destination=destination, options=restore_options)
    else:
        assert job.id is None
    authority.close()
    repository.close()
    for _ in range(100_000):
        restored = job.advance_publication(16)
        if restored is not None:
            break
    assert restored is not None and job.phase == "complete"
    expected = [{"id": 1, "body": "changed"}, {"id": 3, "body": "added"}]
    assert restored.sql("SELECT id, body FROM notes ORDER BY id").rows == expected
    restored.sql("INSERT INTO notes VALUES(4, 'new database')")
    expected.append({"id": 4, "body": "new database"})
    assert job.finish().sql("SELECT id, body FROM notes ORDER BY id").rows == expected
    job.close()
    assert restored.sql("SELECT id, body FROM notes ORDER BY id").rows == expected
    restored.close()
    if persistent:
        reopened = uqa.open_auto(directory / "database", target_options.get("key"))
        assert reopened.sql("SELECT id, body FROM notes ORDER BY id").rows == expected
        reopened.close()


def test_cancelled_jobs_and_invalid_inputs(tmp_path):
    database = uqa.Engine()
    path = tmp_path / "backups"
    repository = uqa.BackupRepository.create(path)
    capture = database.begin_backup(repository)
    assert capture.cancel() is None
    assert capture.phase == "cancelled"
    assert list(repository.list()) == []
    with pytest.raises(RuntimeError):
        capture.finish()
    with pytest.raises(ValueError, match="16 bytes"):
        database.begin_backup(repository, parent=b"short")
    with pytest.raises(RuntimeError):
        database.begin_backup(repository, options=uqa.BackupOptions(maximum_encoded_bytes=0))
    with pytest.raises(ValueError, match="both"):
        uqa.BackupRepository.open(path, key=b"x" * 32)

    ready = database.begin_backup(repository)
    for _ in range(100_000):
        if ready.advance(64, 8192) == "prepared":
            break
    assert ready.phase == "prepared"
    root = ready.id
    ready.close()
    assert repository.publish_prepared(root) == root
    assert repository.publish_prepared(root) == root
    listing = repository.list()
    repository.close()
    assert list(listing) == [root]
    listing.close()
    listing.close()
    with pytest.raises(RuntimeError, match="closed"):
        next(listing)
    repository = uqa.BackupRepository.open(path)
    restore = database.begin_restore(repository, root)
    assert restore.cancel() is None
    assert restore.phase == "cancelled"
    with pytest.raises(RuntimeError):
        restore.finish()

    persistent = uqa.open(tmp_path / "capture.db")
    persistent.sql("CREATE TABLE data(value integer)")
    snapshot = persistent.begin_backup(repository).finish()
    persistent.close()
    directory = tmp_path / "cancelled-restore"
    destination = uqa.RestoreDestination.sqlite(directory)
    restore = database.begin_restore(repository, snapshot, destination=destination)
    operation = restore.id
    restore.close()
    assert database.cancel_restore(repository, operation, destination=destination) is None
    assert not directory.exists()
    database.close()


def test_encrypted_source_requires_explicit_plaintext_export(tmp_path):
    source = uqa.open_encrypted(tmp_path / "source.db", "source key")
    source.sql("CREATE TABLE private_rows(id integer)")
    repository = uqa.BackupRepository.create(tmp_path / "plaintext")
    with pytest.raises(RuntimeError, match="encrypted source"):
        source.begin_backup(repository)
    capture = source.begin_backup(repository, options=uqa.BackupOptions(allow_plaintext_export=True))
    repository.verify(capture.finish())
    source.close()


def test_restore_exposes_authority_runtime_callbacks(tmp_path):
    repository = uqa.BackupRepository.create(tmp_path / "backups")
    source = uqa.Engine()
    source.register_scalar_function("label_id", lambda value: value + 1)
    source.sql("CREATE FUNCTION archived_label(value integer) RETURNS integer LANGUAGE SQL AS $$ SELECT label_id(value) $$")
    backup = source.begin_backup(repository).finish()
    source.close()
    authority = uqa.Engine()
    authority.register_scalar_function("label_id", lambda value: value + 10)
    job = authority.begin_restore(repository, backup)
    authority.close()
    restored = job.finish()
    assert restored.sql("SELECT archived_label(2) AS label").rows == [{"label": 12}]
    restored.close()

def test_restore_admission_inside_a_python_sql_callback(tmp_path):
    # A stalled admission would also hold the SQL caller, so bound the whole process.
    program = dedent("""
        import sys
        import uqa

        repository = uqa.BackupRepository.create(sys.argv[1])
        engine = uqa.Engine()
        engine.sql("CREATE TABLE saved(id integer); INSERT INTO saved VALUES(27)")
        backup = engine.begin_backup(repository).finish()
        jobs = []

        def start_restore():
            jobs.append(engine.begin_restore(repository, backup))
            return 1

        engine.register_scalar_function("start_restore", start_restore)
        assert engine.sql("SELECT start_restore() AS started").rows == [{"started": 1}]
        engine.close()
        restored = jobs[0].finish()
        jobs[0].close()
        assert restored.sql("SELECT id FROM saved").rows == [{"id": 27}]
        restored.close()
    """)
    subprocess.run([sys.executable, "-c", program, str(tmp_path / "backups")], check=True, timeout=30)

def test_copy_and_collect_after_retiring_incremental_roots(tmp_path):
    engine = uqa.Engine()
    engine.sql("CREATE TABLE saved(id integer); INSERT INTO saved VALUES(1)")
    source = uqa.BackupRepository.create(tmp_path / "source", protection_domain=b"D" * 16, key=b"K" * 32)
    parent = engine.begin_backup(source).finish()
    engine.sql("INSERT INTO saved VALUES(2)")
    child = engine.begin_backup(source, parent=parent).finish()
    source.remove(parent)
    for _ in range(100):
        report = source.collect_garbage(3, maximum_workspace_bytes=16 << 20)
        assert report["removed_objects"] + report["removed_staging_files"] <= 3
        if report["remaining_files"] == 0:
            break
    assert report["remaining_files"] == 0
    assert report["completed_backups"] == 1
    source.verify(child)

    destination = uqa.BackupRepository.create(tmp_path / "destination")
    with pytest.raises(RuntimeError):
        source.copy_to(child, destination)
    assert list(destination.list()) == []
    copied = source.copy_to(child, destination, options=uqa.BackupOptions(allow_plaintext_export=True))
    assert copied != child and destination.parent(copied) is None
    source.remove(child)
    assert source.collect_garbage(1024)["remaining_files"] == 0
    source.close()
    destination.verify(copied)
    restored = engine.begin_restore(destination, copied).finish()
    assert restored.sql("SELECT id FROM saved ORDER BY id").rows == [{"id": 1}, {"id": 2}]
    restored.close()
    engine.close()
