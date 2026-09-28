# Bounded notification registry metadata

This boundary supplies fixed-width listener metadata reads, a bounded named-listener lookup and a common Storage summary. SQLite owns physical reads and decoding; Storage owns the key, metadata and fold. It does not yet replace Engine's global coordination path or qualify an owned listener. Those integrations remain in the [notification implementation plan](../plans/0015-sql-notifications-and-sse.md).

## Ordered reads and preservation

Let a listener key be $k=(o,s)$, where $o$ is the owner's sixteen-byte identity and $s$ its unsigned 64-bit session identifier. SQLite stores both key components as blobs, encoding $s$ in big-endian order. For two unsigned integers, the first differing most-significant byte determines the same order as the integers; lexicographic tuple order therefore agrees with the original primary key for every session value, including $2^{64}-1$.

`listener_metadata_after(k)` selects the least key strictly greater than $k$, or the least key when no continuation is supplied. It projects only fixed-width identity, process, wake, transaction and cursor fields; it neither selects nor decodes channel JSON. The query and its row are dropped before return. Within the retained transaction, record the returned key before deleting that row. Induction over subsequent seeks then visits every surviving later key exactly once: strict comparison prevents repetition, and selecting the least greater key prevents omission. No live SQLite cursor remains for deletion to invalidate. The original cancellation control is checked around each operation; cancellation grants no successful record observation.

The named lookup selects only the exact key. With no channel limit it shares the existing legacy JSON materialization semantics. The existing full `listeners()` API keeps its original primary-key order and owned rows, now using the same scalar decoder and unlimited JSON branch. Projection and reconstruction leave every valid stored record's fields unchanged; malformed values still fail. These interfaces perform no publication, cursor advancement or queue deletion themselves.

## Summary algebra

For the unique-key records $L$ of one retained registry state, with next sequence $s_l$, queue position $p_l$, key $k_l$ and selected remote wake port $w_l$, define

$$
S(L)=\left(\min_{l\in L}s_l,\operatorname{argmin}_{l\in L}(p_l,k_l),\bigcup_{l\in L,\,l\text{ remote}}\{w_l\}\right).
$$

The empty summary has absent minima and an empty set. Combining summaries uses minimum and set union. These operations are associative, and the empty summary is their identity. The oldest pair is unique because listener keys are unique in the retained state; summaries from different versions of a key are not combined. `include` first performs its only fallible allocation, then updates both minima. An allocation failure therefore leaves the prior summary unchanged. Induction over successful includes gives exactly $S(L)$; any grouping of the same records has the same result. Comparing $(p_l,k_l)$ preserves the original ordered scan's choice for equally old blocking transactions.

For a wake port $w$, set bit $w\bmod64$ of word $\lfloor w/64\rfloor$. This is a bijection from the complete 16-bit port domain to 1,024 words of 64 bits. Bitwise OR realizes set union, and enumerating words and their least set bits emits each port once in ascending order. The bitmap has 8 KiB of elements when allocated, independent of listener count. The summary retains one fixed metadata record plus this optional bitmap; no channel strings or per-listener vector are retained. These laws concern registry metadata, not document payload merging, scores or ranked-result algebra.

## Channel and resource bounds

For a named lookup with channel limit $M$, reject encoded JSON longer than $B=2+M(6\cdot63+3)$ bytes before constructing strings. The two brackets and each channel's quotes, separator and worst-case escaping of its 63 UTF-8 bytes fit that bound. The visitor admits at most $M$ strings, checks each decoded byte length and rejects an excess element before decoding it. Its fallible header reservation is at most $\min(M,\lfloor B_{\mathrm{actual}}/3\rfloor+1)$; no valid string array in the actual input needs more headers. Decoder scratch and a rejected current string remain bounded by the admitted encoded input. Saturating bound arithmetic prevents wraparound and does not turn a small input with `usize::MAX` into a huge allocation.

These bounds do not include SQLite pages/caches or caller-owned legacy materialization. One indexed seek has fixed-size returned metadata but does not establish a fixed duration for a whole registry pass, a provider I/O cancellation deadline or a complete process-memory profile. The eventual Engine integration must still select live records, preserve its original transaction boundary and apply the resulting minima correctly. The read/fold extension alone changes no committed notification values, SQL duplicate suppression, query operation or UQA composition.
