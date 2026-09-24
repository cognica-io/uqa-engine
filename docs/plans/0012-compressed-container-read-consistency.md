# Compressed container read consistency

Status: Active; deterministic plain and encrypted VFS reproductions fail with SQLite extended error 266 on merged main `ce004f7a`. Issue [#153](https://github.com/cognica-io/uqa-engine/issues/153); branch `fix/compressed-container-read-consistency`. Keep this independent correction behind the sole open PR #152 and committed-notification work #129.

## Failure and external contract

SQLite [pager initialization](https://github.com/sqlite/sqlite/blob/master/src/pager.c) reads the initial database header directly from the opened VFS file before the first shared-lock transition. The VFS decodes a container's chunk positions and authentication metadata during open, but `ContainerFile::read_chunk_from_disk` reopens the pathname later. A concurrent committed compaction replaces that pathname; the old metadata then addresses a different file, causing a checksum or authentication failure surfaced as `SQLITE_IOERR_READ` (266).

The owner regression uses the actual VFS callbacks. It opens a reader, takes the writer's ordinary exclusive lock, publishes enough real updates to trigger a committed compaction, releases that lock and invokes the reader's initial 100-byte header read. Both plain and encrypted variants fail with 266 in Linux Docker. The fixture requires the real compaction generation increment and contains no sleep, random scheduling or performance threshold. After correction, the unlocked initial read must remain bound to its opened file, and the first shared-lock refresh must read the latest committed generation.

## Ownership and implementation

The SQLite provider manifest, existing compression feature/platform boundaries, dependency policy, storage manual and compressed-VFS security contract were inspected. This is a physical container lifecycle defect owned by `uqa-storage-sqlite::compressed_vfs`; neither Engine nor common Storage needs a new dependency or algorithm.

Retain the file descriptor used to authenticate and scan each committed chunk map. Read its chunks through that descriptor instead of resolving the pathname again. Loading, refresh, committed writes and compaction must transfer the descriptor together with their matching metadata. An empty, unpublished container has no committed descriptor. After compaction's rename succeeds, adopt its complete state even if subsequent parent-directory synchronization reports an error. Keep lazy chunk loading and existing cache ownership; do not copy the whole file or disable authentication, identity, generation or trusted-anchor checks.

## Acceptance

Retain the failing owner regression as its own commit. Verify the corrected plain/encrypted schedule, cache-miss reads after compaction, existing refresh/cache behavior, empty first publication, read-only locks, encrypted compaction and directory-sync failure, rollback/journal behavior, and the existing tampering/replay/truncation/anchor suite. Run strict SQLite Clippy and repository dependency, capability, harness, formatting, header and file-limit checks. Update HISTORY, the storage/security contract and this plan with exact results. No timing or allocation benchmark is required.

Initial reproduction: `/private/tmp/uqa-compressed-reader-reproduction.log` contains two executed failures, each expected `SQLITE_OK` and received 266. Raw compiler and runtime logs remain outside Git. The initial #129 process-open failure and its later passing run remain in the issue; later success alone does not establish a correction.
