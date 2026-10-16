# Bit-string input and consumer acceptance

This prerequisite fixes [#914](https://github.com/cognica-io/uqa-engine/issues/914) inside original record 64 and [PR #911](https://github.com/cognica-io/uqa-engine/pull/911). The existing composite-consumer reference keeps all thirteen failing bit/varbit observations until the implementation passes them. A carrier or parser-only change is not completion.

## Ownership and representation

Core owns a packed finite bit sequence, its canonical representation, equality/order/hash and controlled copies. SQL owns BIT/VARBIT identity, declared modifiers, binary/hexadecimal text input, assignment versus explicit casts, operators, physical Datum interpretation and diagnostics. Execution consumes selected SQL equality, hash and retained result metadata; Engine supplies session, provider and reopen adapters. Inspected manifests and the dependency policy allow SQL to use Core; no reverse dependency or new Cargo feature is required.

`BitStringValue` stores most-significant-bit-first bytes and the exact logical bit length. The final byte's unused low bits are zero and no excess bytes are accepted. This retains one byte per eight input bits without conflating a bit string with ordinary text or BYTEA. The standalone owner is implemented first; connecting it to the dynamic `Value` carrier, persistence codecs and SQL remains required. No public SQL support is claimed from the standalone tests.

## Preservation argument

Let $B=\{0,1\}$ and let $B^*$ be the finite bit sequences representable by the length field. For $s\in B^*$ of length $n$, define $P(s)$ as $n$ together with $\lceil n/8\rceil$ bytes, filling each byte from its most significant bit and setting every unused low bit to zero. The inverse reads exactly the first $n$ bits. Therefore $P$ is injective and surjective onto the admitted packed representations: unpacking $P(s)$ yields $s$, and packing an admitted representation's logical bits yields its original bytes and length. Constructors and deserialization reject values outside this image instead of silently normalizing them.

Equality of admitted packed values is equality in $B^*$. Lexicographic byte comparison followed by logical length comparison is the same lexicographic bit order with $0<1$: the first differing logical bit has the greatest positional weight among the remaining bits of its byte; if one sequence is a prefix, zero padding cannot reverse that order, and either the first nonzero suffix bit or the length tie-breaker places the extension after its prefix. Thus the comparison is total, antisymmetric and transitive, and equality agrees with both comparison and hashing. Bounded-chunk comparison uses the same ordered byte partitions and performs no value transformation, so cancellation checks do not change completed observations.

The controlled constructor transfers the already-owned packed allocation and its reservation. Copying admits the destination before allocation and retains that reservation until the copied value is released; failed validation, quota failure and either caller's cancellation release partial owned state. These operations do not mutate their inputs. The standalone carrier adds no operation on document support, payload merging, scoring or ranked results, so those existing UQA carriers and laws remain unchanged. Its later integration into the tagged dynamic-value domain must preserve existing variant tags and equality/hash rules; this obligation is still pending.

SQL length semantics are a separate boundary: a fixed-length assignment requires exact length, a varying-length assignment permits at most the declared bound, and explicit casts can truncate or right-pad according to the target family. These are not equality normalizations and must not change the logical value retained by an ordinary read. The independent PostgreSQL probe also distinguishes the BIT keyword's default length of one from unmodified quoted/catalog `bit`, which has no length constraint. See PostgreSQL's [bit-string contract](https://www.postgresql.org/docs/18/datatype-bit.html) and [type implementation](https://github.com/postgres/postgres/blob/REL_18_STABLE/src/backend/utils/adt/varbit.c).

## Evidence and remaining work

The compact Core fixture `crates/uqa-core/src/types/bit_string/postgresql.json` captures 144 pair comparisons from Docker PostgreSQL 18.4, including empty values, leading zeroes, prefixes, partial final bytes and byte boundaries. It retains the exact query, collector hash and capture hash; raw machine output stays outside Git. Owner tests additionally check comparisons across 4 KiB traversal boundaries, malformed representations, serialization, original byte allowances and both cancellation owners. These finite tests exercise the preservation argument and do not replace it.

| Requirement | Current state | Required exit evidence |
| --- | --- | --- |
| Packed value owner and controlled operations | Implemented; three owner tests pass locally | All 144 independent comparison pairs, canonical constructors and memory/cancellation checks pass. |
| Dynamic Value and provider/spill retention | Pending | Every serialization and controlled-copy path retains length, bytes and distinct identity; malformed input is rejected. |
| Declaration identity and modifiers | Pending; independent PostgreSQL probe captured | BIT, quoted/catalog bit, VARBIT, arrays and composite declarations preserve OIDs/modifiers and exact errors. |
| Literal/input/assignment/casts | Pending; binary/hex and length behavior captured | Input spelling, length validation order, explicit-cast truncation/padding and integer conversions agree with PostgreSQL. |
| Physical values and observing consumers | Pending | Tuple/array encoding, lazy retained reads, equality/order/hash, indexes, concurrent uniqueness and forced-spill consumers agree. |
| Public/provider/binding acceptance | Pending | Original combined matrix passes on all providers and after reopening; bindings preserve the typed value. |
| Automatic regression acceptance | Pending | Actual source-matched Linux and macOS work, memory and output checks pass before merge. |
