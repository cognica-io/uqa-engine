#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Restore an incremental memory backup after its parent has been removed."""

from contextlib import closing
from pathlib import Path
from tempfile import TemporaryDirectory

import uqa


def main() -> None:
    with TemporaryDirectory() as directory:
        with closing(uqa.BackupRepository.create(Path(directory) / "backups")) as repository, closing(uqa.Engine()) as source:
            source.sql("CREATE TABLE accounts (id INTEGER PRIMARY KEY, balance INTEGER)")
            source.sql("INSERT INTO accounts VALUES (1, 90)")
            with closing(source.begin_backup(repository)) as capture:
                full = capture.finish()
            source.sql("UPDATE accounts SET balance = 105 WHERE id = 1")
            with closing(source.begin_backup(repository, parent=full)) as capture:
                child = capture.finish()
            repository.remove(full)
            repository.verify(child)
            with closing(source.begin_restore(repository, child)) as restore:
                restored = restore.finish()
        with closing(restored):
            assert restored.sql("SELECT balance FROM accounts").rows == [{"balance": 105}]


if __name__ == "__main__":
    main()
