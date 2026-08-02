# Compressed VFS security contract

The encrypted compressed SQLite container uses on-disk format v2. Version 2
separates encryption and metadata-authentication keys from one Argon2-derived
key schedule and assigns every container a random file identity.

The format authenticates:

- the complete header, including compression layout, logical length,
  generation, salt, and file identity;
- every chunk entry, including its physical offset, lengths, flags, checksum,
  nonce, generation, and chunk ID, as AEAD associated data;
- every commit record, including logical length, chunk count, generation,
  physical record location, the previous committed-state tag, and every
  pending chunk entry plus its AEAD payload tag.

The loader applies chunk records only when a later authenticated commit covers
their generation. Old-generation records appended after a commit are rejected,
an incomplete next-generation tail is ignored for crash recovery, and a file
truncated below the generation named by its authenticated header is rejected.
The chained commit tags reject history splicing, while the file identity and
per-file derived keys prevent records from being moved between containers.

## Threat boundary

These checks detect modification, relocation, resequencing, replay, history
splicing, and truncation within the visible container. They cannot distinguish
an attacker replacing the entire file with a previously captured, internally
valid snapshot or same-generation fork. That requires an exact trusted state
anchor stored outside the database.

The engine exposes that comparison as an enforceable open contract:

1. After committed writes, call `Engine::compressed_container_anchor(path,
   key)` and persist the returned database identity, generation, and state tag
   in a trusted store outside the database.
2. On the next open, load that value and call
   `Engine::open_compressed_encrypted_with_anchor(...)`.

The VFS requires all three fields to match exactly while opening the SQLite
main file. A separate container, an older or newer snapshot, and a divergent
fork at the same generation are rejected before SQLite reads it. Refresh the
trusted anchor after every committed write. If a database commit becomes
durable but the external anchor update does not, the next open fails closed and
requires an explicit trusted recovery decision. Once a process has registered
an anchor for a path, a later unanchored registration cannot remove it; only a
successfully authenticated newer exact anchor can advance it.

Format v1 did not authenticate metadata or commit records and is deliberately
rejected. Migrate a trusted v1 database with a v1-capable release by exporting
it and importing it into either a v2 container or SQLCipher. Do not use a v1
reader on an untrusted container merely to bypass the rejection.

## Deployment choice

SQLCipher remains the recommended encrypted-storage backend for
security-sensitive deployments because its page format and integrity behavior
have substantially broader deployment and review history. The encrypted
compressed VFS is appropriate only when compression is required and its threat
boundary above is acceptable. A third-party cryptographic review is still
required before treating the custom format as equivalent to an independently
audited storage system.

The adversarial unit suite covers header, chunk-entry, and commit modification;
same-file and cross-file replay; same-generation forks; commit-chain splicing;
authenticated-generation truncation; crash tails; wrong keys; and encrypted
compaction.
