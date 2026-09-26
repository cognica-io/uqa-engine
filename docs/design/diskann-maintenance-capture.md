# DiskANN maintenance capture

Storage owns exact outstanding-change accounting and one-use reconstruction from the same committed canonical snapshot. `PersistentStorageBackend::diskann_maintenance_source` supplies this capability through common Key/Value storage and native SQLite; redb uses the common adapter. Execution remains responsible for admission thresholds, scheduling and original transaction completion. The existing database worker now uses this capability for automatic graph reconstruction, while Engine retains the host session, policy and resource adapters.

## One captured authority

The provider admits a source only on an inactive bound session. It retains the actual catalog/index definition, canonical tensors, immutable change journal and selected physical generation on the same committed view. The existing `KeyValueDiskANNPruner` verifies that generation's complete origin artifact once. Opening this source does not prepare the old graph or resident PQ codes. Source metadata, retained names and origin reads keep the original memory allowance and cancellation signal.

`statistics` reads both discovery keys and canonical evidence from that retained view. This differs from [journal pruning](diskann-journal-pruning.md), whose finite discovery may be old but whose deletion evidence must come from the current publishing transaction. A missing value for a key on the exact census view is corruption; a key removed by a peer after pruning discovery is an ordinary consumed pruning slot. Both operations share the complete-origin classification and reject mismatched version envelopes or dimensions.

Each census request examines at most 64 immutable mutation keys. A zero limit, nonadvancing provider cursor or continuation from another generation fails. A continuation belongs to its original source: a fresh capture starts without a cursor even if it selects the same generation. Callers accumulate each page once with checked arithmetic and discard the entire census on error. Neither page completion nor census completion publishes writes.

## Exact additive accounting

Let $S$ be the captured canonical/journal view, $G$ its selected generation, $K_S$ its finite set of journal identities, $J_S(d,v)$ the complete journal origin, $O_S(d)$ the current origin in that same view and $O_G(d)$ the published complete origin. Define the outstanding identities by

$$
A_{S,G}=\{(d,v)\in K_S\mid J_S(d,v)=O_S(d)\land J_S(d,v)\ne O_G(d)\}.
$$

An origin includes its immutable mutation identity, tensor count, dimensions and fingerprint. Same-identity disagreements fail; revision ordering or a digest alone never establishes coverage. Committed mutation identities are unique and never reused after undo, so at most one identity per document belongs to $A_{S,G}$. Empty replacements remain explicit origins and contribute one document even though they contain zero vectors.

For dimension count $D$ and tensor length $n(d,v)$, the reported triple is

$$
T(S,G)=\sum_{(d,v)\in A_{S,G}}\bigl(1,\ n(d,v),\ 4D\,n(d,v)\bigr).
$$

The components count documents, vectors and logical raw `f32` coordinate bytes. Vector bytes are not journal record sizes, compressed-file usage or retained MVCC history. Resource reclamation and physical storage accounting keep their existing owners.

Componentwise addition on nonnegative integer triples has identity $(0,0,0)$ and is associative and commutative. Disjoint census pages therefore produce the same exact triple regardless of how their totals are combined. The implementation rejects overflow instead of wrapping or saturating. Since every admitted page advances over distinct ordered keys in the same finite $K_S$, at most $\lceil |K_S|/64\rceil+1$ successful requests finish the census, including a possible terminal empty page. This is a bound on keys, not constant work for arbitrarily large canonical tensors: origin validation retains its existing bounded streaming work.

The durable inputs are the already evaluated per-mutation records written atomically with canonical tensors. Independent document writers append distinct identities and retain existing common MVCC merge behavior; they do not update a shared DiskANN counter or require a whole-index expected-version condition. The census folds those durable records and is not a new commit-time counter format. No provider format upgrade is required.

## Consuming reconstruction and completion

`DiskANNMaintenanceSource::rebuild(self: Box<Self>, ...)` consumes exactly the source whose census was observed. It requires the original memory allowance and cancellation signal, then passes its retained canonical/catalog capture to the existing bounded provider `rebuild_source` owner. The caller supplies the shared encrypted temporary allowance and effective index options; existing catalog validation rejects different persistent parameters. The old origin-reader workspace closes before new construction begins.

The caller must first begin a transaction on that source's original session. Reconstruction stages the completed successor, original catalog requirements and original expected head in that transaction. It never commits, rolls back unrelated caller effects or silently refreshes its input. Admission/build failure preserves preceding private writes, and a competing head or catalog publication rejects the old capture. Source consumption prevents invoking reconstruction twice through the same capability; receipt recovery remains the caller's existing original-outcome completion operation and must not recapture and replay a build.

The constructed generation's complete coverage describes $S$. Any mutation committed later has a distinct origin and remains in the exact change path until a later generation covers it. Deleting covered/obsolete journal rows uses the existing guarded pruner after confirmed publication. Older queries retain their original canonical, generation and journal history through MVCC leases. Thus construction preserves canonical tensors, original raw-score semantics and visibility; ANN candidate membership may change with the rebuilt graph under the existing access-method contract.

Cancellation or quota failure invalidates the census/build operation. The source cannot substitute a larger fresh memory allowance or a different cancellation signal. Independent provider cleanup controls still allow rollback after original write cancellation. Physical orphan reclamation, uncertain receipt retention and staged-resource ownership remain governed by the [generation publication](diskann-generation-publication.md) and [physical generation](diskann-key-value-generations.md) contracts.

## Automatic reconstruction

Execution's `DiskANNJournalMaintenance::with_rebuilds` retains the host's shared encrypted-temporary allowance and a positive `DiskANNRebuildPolicy`; the original `new` constructor remains available for standalone journal pruning. The database worker always enables reconstruction. The default soft thresholds are 1,024 current uncovered documents or 64 MiB of logical raw vector bytes. They are host policy, not SQL index parameters, write limits or claims about an optimal workload setting.

For admitted limits $L_d>0$ and $L_b>0$, the completed census selects reconstruction exactly when

$$
\operatorname{eligible}(S,G)=(T_1(S,G)\ge L_d)\lor(T_3(S,G)\ge L_b).
$$

The census is read-only and finishes on its original finite key set before this decision. A false result transfers the same source to the existing current-evidence pruner. A true result consumes that source in one construction on its independent provider session. Policy replacement affects later admissions and resets unchanged-version suppression; a previously admitted source keeps its policy, original controls and allowance. Thus policy changes cannot combine statistics from one capture with another build or silently enlarge a resource budget.

The transaction owner represents evaluated work as either a pruning page or one completed build. It stores that result before asking the provider to commit. Only confirmed commit increments the rebuild count; uncertain or committed-but-unacknowledged completion retains the same attempt, even after cancellation. Definite rejection and failed construction enter cleanup, preserving the original operation error. A consumed build never returns to the evaluation state: the only transition after successful publication is to a finished job. Therefore repeated completion steps cannot duplicate construction, user expressions or publication effects. Read-only pruning pages retain their existing confirmed-rollback rule.

After confirmed reconstruction, a later catalog pass captures the actual successor and prunes covered changes. Commits during the previous pass and policy changes cannot be hidden by unchanged-version coalescing. The host admits one index at a time from a finite retained catalog. It immediately continues successful steps while that pass has pending work, so a large census is not delayed by one timer interval per page. Idle work and errors use the existing approximate one-second polling interval; no extra maintenance thread or unbounded retry loop is introduced. A construction step runs the existing bounded Storage builder and may take longer than a page; it retains cancellation checks and the supplied memory/temporary limits throughout.

The new state machine changes representation and maintenance timing, while the [publication proof](diskann-generation-publication.md) preserves canonical visibility for both the captured corpus and later mutations. Every query still scores its selected documents with the same canonical tensor reduction and existing posting construction. It introduces no new collision merge, score conversion or probability transform; rebuilt ANN candidate membership retains the existing access-method contract. Old query sources retain their original MVCC leases independently of maintenance completion.

`DiskANNMaintenanceStatus` distinguishes complete captured censuses from confirmed rebuilds and pruning effects. Its latest census names the original generation whose changes were measured; it does not claim that this is the successor generation or the current live corpus. Engine exposes these values and the process-local, database-shared policy through retained-state adapters. Algorithms, eligibility, finite-pass coordination and original-outcome handling remain in their existing Storage/Execution owners.

## Provider acceptance

One shared conformance schedule runs through common MVCC, native SQLite, SQLite Key/Value and redb, with all four SQLite file modes and actual cold reopen. It creates 75 journal entries containing repeated tensor replacements, an explicit empty replacement and superseded versions; the retained two-page census has the literal total $(3,3,24)$. Peer writes between pages cannot change that total or extend its captured 75 keys. Rebuilding that source leaves the later two documents uncovered, with literal total $(2,2,16)$, while old and current query scores retain their separately expected values.

The same schedule checks original quota exhaustion, a substituted cancellation signal, a missing caller transaction, resident-generation admission failure preserving outer writes, stale-generation cursor rejection, competing publication, complete pruning, original cancellation with successful cleanup, temporary-file release and cold restore of the actual selected head. Existing pruning and completion tests remain applicable because both paths share origin classification and provider transaction ownership. Execution completion tests additionally cover threshold decisions, cancellation, lost/committed replies, definite rejection and failed cleanup without recapture or repeated construction. Engine schedules exercise real automatic rebuilding, late writes, policy replacement, shared temporary rejection, empty successors and cold reopen on all three persistent providers.
