# Selected range types in ordered peers

This correction addresses [#948](https://github.com/cognica-io/uqa-engine/issues/948), reproduced after the scalar equality-key correction #946. It remains a separate delivery from #947 and #946. Original #638 record 65 stays open for declarations, selected support invocation, constructors, lifecycle and provider/binding acceptance.

## Reproduction and ownership

PostgreSQL 18.4 treats `[1.0,3.00)` and `[1.00,3.0)` as equal numeric ranges. Actual Engine SQL returned only one row for `FETCH FIRST 1 ROW WITH TIES`, separated their window partitions and assigned different ranks/peer frames. The same five failures occurred for `nummultirange`. The compact independent PostgreSQL fixture records all ten queries and expected counts; the pre-fix Engine run fails all ten. These are deterministic output differences, with no timing claim.

Execution's sort retained the selected expression types, while LIMIT, window partition boundaries and window peer boundaries compared bare values. Because admitted native ranges use text carriers, those three consumers compared spelling instead of the type's equality. SQL already owns selected-type equality; Execution now retains each key's resolved type once per operator open or window pass and passes it to that existing interface. Engine changes consist only of public SQL acceptance tests. No dependency, provider format or stored representation changes.

## Preserved semantics

For selected key types $T_1,\ldots,T_n$, define peer equality $P(a,b)=\bigwedge_i E_{T_i}(a_i,b_i)$, using the existing SQL equality and its NULL-equals-NULL grouping rule. The sorted stream and all three boundary consumers now observe those same selected types. Replacing a numeric range spelling by an equal spelling therefore leaves its partition and peer group unchanged. LIMIT retains the complete boundary group; rank and frame evaluation retain their existing multiplicity and position rules. Domains observe their admitted base values, while ordinary TEXT keeps string equality. This corrects type transport without adding an algebraic operator or changing row payloads, scores, retained encodings or function invocation semantics.

## Verification and delivery

Owner checks cover scalar ranges and multiranges, domains, ordinary TEXT, NULLs, multi-column peers, in-memory and forced-spill window rows, and LIMIT's forward, backward and rewind paths. Public SQL compares all ten reproduced queries with the independent PostgreSQL output. All nine selected Execution checks pass, including the existing large disk-backed partition check. All ten public SQL results match after the final sort-key method extraction. Strict Execution library/test Clippy, formatting, the 109-edge dependency policy, Engine ownership, single-harness inventory, headers and file ceilings pass. The automatic manifest contains 830 checks / 1,226 cases, adding the two owner checks and public SQL check. Exact-source automatic Linux/macOS bounded-work, memory, output and independent-process qualification remains required before delivery. The original task records remain unchanged in successor PR bodies; #948 closes with its verified corrective PR.
