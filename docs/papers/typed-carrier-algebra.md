# A Typed Carrier Algebra for Unified Query Execution

## Relational, Text, Vector, Graph, and Probabilistic Retrieval in One Runtime

**Jaepil Jeong**

Cognica, Inc.

*jaepil@cognica.io*

Preproduction manuscript, August 3, 2026

## Abstract

Systems that combine SQL, full-text retrieval, vector search, graph traversal, and probabilistic ranking are often described as “unified” because they share an API or because every intermediate result is forced into one container. Neither condition is a sufficient mathematical foundation. A single container can carry several observably different structures: set membership, tuple multiplicity, scores, positional payloads, graph-match context, and rank order. Algebraic laws valid for one of these structures can be unsound for another.

This paper develops an implementation-grounded framework in which unification occurs through a **typed family of carriers** and a common planning and execution calculus. Finite document sets carry Boolean algebra; finite-support relations carry semiring addition and multiplication; decorated postings carry explicitly non-Boolean collision policies; ranked views own ordering and top-$k$; generalized relations preserve join-tuple identity; graph postings pair document support with invariant-checked graph context; and aggregate states carry monoids. Operators compose only when their input and output carriers agree, while explicit adapters mark every intentional loss or encoding.

The framework yields four practical results. First, it states exactly which rewrites—idempotence, absorption, filter pushdown, threshold merging, join reordering, pattern fusion, and aggregate decomposition—are valid under which observations. Second, it separates raw retrieval scores, prior-free evidence, priors, and posterior probabilities, giving an exact single-prior Bayesian fusion rule while distinguishing robust ranking heuristics. Third, it integrates graph traversal and regular path queries without claiming an isomorphism between arbitrary graphs and document sets; a versioned $\Phi$ codec is proved only as a lossless representation change over a constrained graph-posting carrier. Fourth, it connects the algebra to a working Rust engine whose statements lower to one typed plan hierarchy and whose specialized access paths retain their native carriers.

The result is a broad but bounded claim: relational, text, vector, graph, and probabilistic retrieval can share a principled optimizer and runtime without erasing the semantic distinctions on which correctness depends.

**Keywords:** query algebra, semiring relations, information retrieval, vector search, graph query, probabilistic fusion, query optimization, embedded database

## 1. Introduction

Modern applications increasingly ask one query to cross several data paradigms. A relational predicate narrows a corpus; full-text retrieval identifies lexical evidence; vector search supplies semantic candidates; a graph traversal adds structural context; ranking and aggregation produce the final answer. Deploying a separate engine for each stage makes the application responsible for identity mapping, transaction boundaries, score interpretation, and cross-system optimization.

A unified runtime is attractive, but the mathematical difficulty is not syntax. The difficulty is that the paradigms expose different observable structures:

- SQL commonly evaluates bags of typed rows and must preserve null and duplicate semantics.
- Boolean retrieval reasons about membership in a finite document universe.
- Weighted retrieval associates values with documents and may combine them additively or multiplicatively.
- Posting indexes carry positions, fields, bounds, and scores whose merge policy is observable.
- Top-$k$ depends on order and can discard support.
- Graph matches carry vertex, edge, name, path, and temporal context.
- Joins produce tuples rather than single document identifiers.
- Probabilistic values are meaningful only when priors and evidence domains are not mixed.

Treating all of these as one undifferentiated “posting-list algebra” overstates the common structure. In particular, a posting list with payloads is not bijective with a set of documents: projecting to document identifiers loses information. A payload merge that adds scores is not idempotent. A right-biased field merge is not commutative. A ranked top-$k$ is not a Boolean homomorphism. An arbitrary graph is not isomorphic to a document set.

These observations do not defeat unification. They locate it more precisely. We propose that a heterogeneous query system is unified when:

1. every intermediate value has an explicit carrier and observation;
2. every operator has a typed signature between carriers;
3. conversions are named and their information loss or round-trip law is stated;
4. optimizer rewrites are indexed by the laws of the carrier they transform; and
5. all statement forms participate in one plan and execution hierarchy.

This formulation is both broader and more defensible than container uniformity. It supports cross-paradigm composition while permitting each domain to retain the structure needed for correct execution.

### 1.1 Contributions

This paper makes the following contributions.

1. **A carrier-indexed query algebra.** We define finite Boolean support, semiring-valued relations, decorated postings, ranked views, tuple relations, graph postings, row bags, and aggregate states as separate but composable carriers.
2. **A law-preserving support projection.** We show that decorated posting merges project to document-set union and intersection while proving why the posting values themselves need not satisfy Boolean idempotence or commutativity.
3. **Typed joins and aggregations.** We express relational and cross-paradigm joins over tuple identities and state the exact monoid condition required for parallel aggregation.
4. **A score-domain calculus.** We separate raw BM25 scores, evidence logits, prior logits, and posterior probabilities. Under explicit conditional-independence assumptions, signed evidence adds and the prior enters exactly once.
5. **A graph extension without a universal-isomorphism claim.** Graph query results retain graph context in a dedicated carrier. A versioned $\Phi$ codec has a constrained round-trip theorem; graph operations and regular path queries remain graph-native.
6. **A law-indexed optimizer and unified plan model.** We connect formal laws to executable rewrite guards and to a runtime in which relational, retrieval, graph, and command plans share one exhaustive planning and execution path.
7. **An evidence discipline.** Algebraic correctness, empirical calibration, approximate-index recall, persistence, and performance are treated as different claims with different validation methods.

### 1.2 Scope

The model concerns finite database snapshots and an embeddable single-node runtime. It includes relational SQL, text and vector retrieval, graph queries, ranking, joins, aggregation, and transactional persistence. It does not claim distributed query execution, universal graph/document equivalence, calibration without labeled evaluation, or performance superiority without reproducible measurements.

### 1.3 Relation to the Earlier Unified-Algebra Formulation

This manuscript consolidates and revises the original unified-query-algebra formulation [18] and its graph-data extension [19] in light of the executable system. It retains their central objective—one compositional framework for relational, textual, vector, and graph queries—and preserves the useful Boolean-support, cross-paradigm join, aggregate, hierarchical-value, traversal, and regular-path constructions. It changes the foundation where implementation exposed stronger semantic requirements.

The principal revision is to replace the proposed universal posting-list representation with a typed family of carriers. The earlier document-set/posting-list bijection is retained only as the support/lift round trip for default-decorated postings; it is not asserted for postings with observable payload. Likewise, the graph extension's broad graph/document isomorphism is replaced by a lossless, versioned codec theorem over the invariant-checked graph-posting carrier. Score combination is divided into exact single-prior Bayesian evidence fusion and explicitly heuristic pooling, while optimizer identities are stated relative to the observation under which they are sound. These are not reductions in the system's compositional ambition. They are the conditions that let the broader claim survive contact with tuple identity, bags, payload collisions, ranking, graph context, and persistent execution.

## 2. Why One Universal Posting Carrier Is Insufficient

Let $U$ be a finite universe of document identifiers and let $\Pi$ be a payload domain. A decorated posting value can be modeled as a finite partial map

$$
P : U \rightharpoonup \Pi.
$$

Its support is

$$
\operatorname{supp}(P)=\{d\in U\mid P(d)\text{ is defined}\}.
$$

The map $\operatorname{supp}:P_\Pi(U)\to\mathcal P(U)$ is generally many-to-one. Two posting values with the same identifiers but different positions, scores, or fields have the same support. If $\operatorname{lift}(D)$ assigns a default payload to every $d\in D$, then

$$
\operatorname{supp}(\operatorname{lift}(D))=D,
$$

but in general

$$
\operatorname{lift}(\operatorname{supp}(P))\neq P.
$$

Thus document sets are a **retract** of default-decorated postings, not a representation isomorphic to all decorated postings.

This distinction matters operationally. Suppose a collision merge on payloads adds scores, unions positions, and gives the right operand precedence for colliding fields. For a posting $P$ containing a scored document,

$$
P\mathbin{\widetilde\cup}P\neq P
$$

because its score is doubled. For two postings $P,Q$ with different values for the same field,

$$
P\mathbin{\widetilde\cup}Q\neq Q\mathbin{\widetilde\cup}P
$$

under full payload equality. Nevertheless their supports can still satisfy ordinary set union. The optimizer must therefore know whether it is observing membership or the full decorated value.

The same problem appears elsewhere:

- a SQL bag cannot be reduced to a set without losing multiplicity;
- a join result cannot be reduced to one document identifier without losing tuple identity;
- a graph match cannot be reduced to document support without losing matched vertices and edges; and
- a rank order cannot be reconstructed from an unordered support set.

The corrected foundation is consequently many-sorted: common structure is shared where it exists, and explicit boundaries are retained where it does not.

## 3. Snapshot and Carrier Model

### 3.1 Database snapshots

A database snapshot is written

$$
\Sigma=(U,\mathcal T,\mathcal I,\mathcal G,\Theta,\mathcal C),
$$

where $U$ is the finite identifier universe, $\mathcal T$ is relational and document data, $\mathcal I$ is the family of physical indexes, $\mathcal G$ is the named-graph state, $\Theta$ is scoring and model metadata, and $\mathcal C$ is catalog state. A read operator is pure relative to a pinned $\Sigma$. A state-changing operator is modeled separately in Section 9.

### 3.2 Core carriers

The logical and physical framework uses the following carrier family.

| Carrier | Mathematical form | Primary observation |
| --- | --- | --- |
| Document support $D_U$ | $\mathcal P(U)$ | identifier membership |
| $K$-relation $R_K(U)$ | finite $r:U\to K$ | identifier-value pairs |
| Decorated posting $P_\Pi(U)$ | finite $U\rightharpoonup\Pi$ | identifiers and complete payloads |
| Ranked view $V_s(P)$ | deterministic order over a posting | score order and top-$k$ |
| SQL row bag $B_\Gamma$ | $\mathbb N^{(\mathrm{Row}_\Gamma)}$ | schema, multiplicity, values, and requested order |
| Tuple relation $J_{\Gamma_1,\ldots,\Gamma_n}$ | finite relation over identifier tuples | complete tuple identity |
| Graph posting $GP(U)$ | posting plus graph-context side map | support, payload, and graph context |
| Aggregate state $A_M$ | element of a monoid $M$ | finalized aggregate value |

The list is intentionally not a subtyping hierarchy. A value may be converted only through a named map whose contract is known.

### 3.3 Typed operators

Each operator has a signature. Representative examples are:

$$
\begin{aligned}
\operatorname{term}_{f,q} &: \Sigma \to P_\Pi(U),\\
\operatorname{filter}_{p} &: D_U\to D_U,\\
\operatorname{vector}_{f,v,\tau} &: \Sigma\to P_\Pi(U),\\
\operatorname{knn}_{f,v,k} &: \Sigma\to V_s(P_\Pi(U)),\\
\operatorname{rank}_s &: P_\Pi(U)\to V_s(P_\Pi(U)),\\
\operatorname{top}_k &: V_s(P_\Pi(U))\to P_\Pi(U),\\
\operatorname{join}_{\theta} &: J_A\times J_B\to J_{A\cup B},\\
\operatorname{traverse}_{R} &: \mathcal G\to GP(U),\\
\operatorname{aggregate}_{h} &: B_\Gamma\to A_M.
\end{aligned}
$$

An operator composition $g\circ f$ exists only when the output carrier of $f$ is the input carrier of $g$, or when an explicit adapter is inserted. The pure fragment therefore forms a category $\mathsf{UQA}$: carriers are objects, total deterministic operators are morphisms, identities are carrier identities, and composition is ordinary function composition. This modest categorical statement is sufficient. A value-only adapter is not called a functor unless an operator mapping exists and identity and composition preservation have been established.

### 3.4 Carrier-relative equivalence

For every carrier $C$, let $\operatorname{obs}_C$ expose the values that are semantically visible at that boundary. Queries $q_1,q_2:X\to C$ are equivalent under snapshot $\Sigma$ when

$$
q_1\equiv_C q_2
\iff
\operatorname{obs}_C(q_1(\Sigma,x))
=
\operatorname{obs}_C(q_2(\Sigma,x))
\quad\text{for every valid }x.
$$

Equality of support is therefore weaker than equality of decorated postings, and equality of an unordered tuple set is weaker than equality of an ordered SQL result. This indexed equivalence is the basis of safe rewriting.

## 4. Algebraic Laws by Carrier

### 4.1 Finite Boolean algebra of document support

Fix a finite universe $U$. For $A,B\subseteq U$, define

$$
A\vee B=A\cup B,\qquad
A\wedge B=A\cap B,\qquad
\neg_U A=U\setminus A.
$$

**Theorem 4.1 (Finite support algebra).**
$(D_U,\vee,\wedge,\neg_U,\varnothing,U)$ is a complete Boolean algebra.

**Proof.** The power set of a fixed set is a Boolean algebra under union, intersection, and relative complement. Because $U$ is finite, every family of subsets has a union and intersection in $D_U$; hence the lattice is complete. $\square$

The qualification “fixed universe” is essential. A complement is not an intrinsic property of a posting; it is relative to the snapshot or relation being queried. In SQL, complement lowering also requires a two-valued predicate. A nullable comparison under NOT remains in the three-valued row evaluator rather than being rewritten as set complement.

### 4.2 Finite-support semiring relations

Let $K=(K,\oplus,\otimes,0,1)$ be a semiring. Relative to finite $U$, define

$$
R_K(U)=\{r:U\to K\}
$$

with pointwise operations

$$
(r\oplus s)(d)=r(d)\oplus s(d),\qquad
(r\otimes s)(d)=r(d)\otimes s(d).
$$

**Theorem 4.2 (Pointwise relation semiring).**
$R_K(U)$ is a semiring under the pointwise operations, with the constant-zero and constant-one relations as identities.

**Proof.** Every semiring axiom holds at each $d\in U$, so it holds extensionally for the functions. $\square$

Sparse storage omits entries equal to $0$. For the Boolean semiring

$$
K_\mathbb B=(\{\bot,\top\},\lor,\land,\bot,\top),
$$

the characteristic map $\chi:D_U\to R_{K_\mathbb B}(U)$ satisfies

$$
\chi(A\cup B)=\chi(A)\oplus\chi(B),\qquad
\chi(A\cap B)=\chi(A)\otimes\chi(B).
$$

For the log semiring, $0_K=-\infty$, $1_K=0$, addition is log-sum-exp, and multiplication is ordinary addition in log space. This supports stable combination of non-negative weights without attributing those laws to a payload container.

Support projection from a general semiring relation needs two familiar qualifications:

$$
\operatorname{supp}(r\oplus s)
=\operatorname{supp}(r)\cup\operatorname{supp}(s)
$$

when $K$ is zero-sum-free, and

$$
\operatorname{supp}(r\otimes s)
=\operatorname{supp}(r)\cap\operatorname{supp}(s)
$$

when $K$ has no zero divisors. The Boolean and non-negative log semirings used here satisfy these conditions.

### 4.3 Decorated postings and support homomorphisms

Let $m:\Pi\times\Pi\to\Pi$ be a collision policy. Define decorated support-union and support-intersection by

$$
(P\mathbin{\widetilde\cup_m}Q)(d)=
\begin{cases}
m(P(d),Q(d)), & d\in\operatorname{supp}(P)\cap\operatorname{supp}(Q),\\
P(d), & d\in\operatorname{supp}(P)\setminus\operatorname{supp}(Q),\\
Q(d), & d\in\operatorname{supp}(Q)\setminus\operatorname{supp}(P),
\end{cases}
$$

and by retaining only the first case for $\widetilde\cap_m$.

**Proposition 4.3 (Support preservation).**

$$
\begin{aligned}
\operatorname{supp}(P\mathbin{\widetilde\cup_m}Q)
&=\operatorname{supp}(P)\cup\operatorname{supp}(Q),\\
\operatorname{supp}(P\mathbin{\widetilde\cap_m}Q)
&=\operatorname{supp}(P)\cap\operatorname{supp}(Q).
\end{aligned}
$$

**Proof.** The constructors include exactly the identifiers in the corresponding set operation; $m$ changes only the payload at an already included identifier. $\square$

No Boolean law for the full posting value follows from Proposition 4.3. If $m$ adds scores, then $m(p,p)\neq p$ in general. If it gives one side field precedence, then $m(p,q)\neq m(q,p)$ in general. Consequently:

- membership-only subtrees may use Boolean idempotence and absorption;
- score-bearing or field-bearing subtrees may not be deduplicated merely because they are structurally equal; and
- set-level optimizer proofs must name support equality rather than silently using full-value equality.

### 4.4 Ranking is a separate algebra

For a posting $P$, a scoring function $s:P\to\mathbb R$, and a deterministic tie key, $\operatorname{rank}_s(P)$ is a total order. The selection $\operatorname{top}_k$ returns at most $k$ entries and then may be re-materialized in identifier order.

Top-$k$ is deterministic, but it is neither a Boolean operation nor generally monotone with respect to support inclusion: adding a high-scoring document may evict an existing result. Therefore

$$
\operatorname{top}_k(A\cup B)
\neq
\operatorname{top}_k(\operatorname{top}_k(A)\cup B)
$$

in general. Pushing top-$k$ below a Boolean or fusion parent requires a separate threshold algorithm and a valid upper bound. In the implementation described here, exact WAND and Block-Max WAND are used only where the complete score is monotone in safe per-term bounds; duplicate query terms remain distinct, and a Boolean or fusion parent blocks leaf truncation.

## 5. Joins, Bags, Aggregates, and Hierarchical Values

### 5.1 Tuple identity and semiring joins

Let schemas $A$ and $B$ share compatible attributes. A $K$-annotated relation over schema $A$ is a finite function

$$
R:\mathrm{Tuple}(A)\to K.
$$

The natural join is

$$
(R\bowtie S)(t)
=
R(t|_A)\otimes S(t|_B)
$$

for compatible tuples $t$, with zero assigned otherwise. Projection to schema $C\subseteq A$ sums annotations:

$$
(\pi_C R)(u)=
\bigoplus_{t:t|_C=u}R(t).
$$

This recovers set semantics with $K=\mathbb B$ and bag semantics with $K=\mathbb N$. It also gives the standard distributive law

$$
R\bowtie(S\oplus T)
=(R\bowtie S)\oplus(R\bowtie T)
$$

by semiring distributivity.

The result identity is the full tuple $t$, not an invented scalar document identifier. A generalized posting representation can therefore sort and deduplicate identifier tuples while retaining their payload. In a SQL row pipeline, multiplicities remain in the bag carrier; in cross-paradigm access joins, the generalized tuple carrier provides set-valued tuple identity. The two are connected at an explicit materialization boundary rather than conflated.

Inner equijoins are associative and commutative up to schema-preserving tuple reordering. Outer joins, lateral dependencies, null-extension, order-sensitive operations, and volatile expressions do not inherit those reorderings. A join optimizer may flatten and reorder only an inner region whose predicates and tuple projections meet the necessary conditions.

### 5.2 Cross-paradigm joins

Text, vector, and graph joins are predicates over typed tuple components:

$$
\begin{aligned}
J_{\mathrm{text},\tau}
&=\{(l,r)\mid \operatorname{sim}_{\mathrm{text}}(l.f,r.g)\ge\tau\},\\
J_{\mathrm{vec},\tau}
&=\{(l,r)\mid \operatorname{sim}_{\mathrm{vec}}(l.v,r.w)\ge\tau\},\\
J_{\mathrm{graph}}
&=\{(l,r)\mid r.id\in\operatorname{traverse}(G,l.id,R)\}.
\end{aligned}
$$

Their shared structure is tuple formation and predicate evaluation; their candidate generation and cost are domain-specific. Preserving $(l.id,r.id)$ lets later filters, projections, and ranking identify both sides without encoding an enumeration position as a document.

### 5.3 Mergeable aggregation

An aggregate is parallelizable when it factors through a monoid

$$
(M,\odot,e).
$$

Let $h:X\to M$ lift one input and let

$$
\operatorname{fold}(x_1,\ldots,x_n)
=h(x_1)\odot\cdots\odot h(x_n).
$$

**Theorem 5.1 (Partition decomposition).**
For a disjoint bag partition $X=X_1\uplus X_2$,

$$
\operatorname{fold}(X)
=\operatorname{fold}(X_1)\odot\operatorname{fold}(X_2).
$$

**Proof.** Reassociate the fold using associativity, inserting $e$ for an empty partition. Commutativity is additionally required if the physical partition may reorder inputs and the aggregate is declared order-insensitive. $\square$

COUNT and SUM use additive monoids. AVG uses the product state $(\mathrm{sum},\mathrm{count})$ and finalizes only after partial states are merged. Ordered-set aggregates are not covered unless their state explicitly preserves the required order.

### 5.4 Hierarchical values

Hierarchical data is modeled recursively:

$$
\mathrm{Value}
=
\mathrm{Atom}
+\mathrm{List}(\mathrm{Value})
+\mathrm{Map}(\mathrm{String},\mathrm{Value}).
$$

A path $p$ defines a partial evaluation

$$
\operatorname{eval}_p:\mathrm{Value}\to \mathrm{Option}(\mathrm{Value}).
$$

A deterministic path filter distributes over bag union because it evaluates each row independently. Path projection and unnesting have different carriers: projection changes row values, while unnesting changes multiplicity and therefore belongs to the row-bag algebra. Idempotence of projection is claimed only for a normalized representation whose output paths are stable under repeated evaluation; it is not inferred merely from the word “projection.”

## 6. Retrieval Scores and Probabilistic Evidence

The score-domain treatment builds on the Bayesian hybrid-search framework [20] while narrowing its claims at implementation boundaries: unlabeled monotone mappings remain score transforms until labeled validation, and exact signed-evidence fusion remains distinct from robust positive-evidence pooling.

### 6.1 Raw text scores

For query terms $q$, a BM25-style score has the additive form

$$
S_{\mathrm{BM25}}(q,d)
=
\sum_{t\in q}
\operatorname{IDF}(t)
\frac{f(t,d)(k_1+1)}
{f(t,d)+k_1\left(1-b+b\,|d|/\operatorname{avgdl}\right)}.
$$

The implementation uses a numerically stable equivalent and a Robertson–Sparck Jones IDF. The complete raw query score belongs to the carrier

$$
S_{\mathrm{raw}}\in\mathbb R,
$$

not to a probability domain. Term contributions are summed before a query-level calibration transform is applied. This order matters: applying a sigmoid independently to every term and then adding the outputs is not equivalent.

For parameters $\alpha>0$ and $\beta\in\mathbb R$, the monotone transform

$$
c_{\alpha,\beta}(S)
=
\sigma\!\left(\alpha(S-\beta)\right)
$$

maps a complete raw score into $[0,1]$. Monotonicity preserves within-query order and permits an upper bound $B$ on the raw score to be mapped to $c_{\alpha,\beta}(B)$. It does not, by itself, prove empirical calibration. If $\alpha$ and $\beta$ are estimated without relevance labels, the result is a bounded score transform until reliability is evaluated on held-out judgments.

The public mathematical boundary therefore distinguishes:

$$
\begin{aligned}
\mathrm{RawBm25Score} &\subset \mathbb R,\\
\mathrm{EvidenceLogit} &\subset \mathbb R,\\
\mathrm{PriorLogit} &\subset \mathbb R,\\
\mathrm{PosteriorProbability} &\subset [0,1].
\end{aligned}
$$

The wrappers have the same machine representation but different admissible operations.

### 6.2 Exact single-prior fusion

Let $R\in\{0,1\}$ denote relevance. For conditionally independent signals $x_1,\ldots,x_n$, define prior-free likelihood-ratio evidence

$$
e_i
=
\log
\frac{p(x_i\mid R=1)}
{p(x_i\mid R=0)}
$$

and a prior logit

$$
\lambda_0
=
\log\frac{p(R=1)}{1-p(R=1)}.
$$

**Theorem 6.1 (Bayesian evidence fusion).**
Under conditional independence given $R$,

$$
p(R=1\mid x_1,\ldots,x_n)
=
\sigma\!\left(\lambda_0+\sum_{i=1}^{n}e_i\right).
$$

**Proof.** Bayes’ rule gives posterior odds as prior odds multiplied by each likelihood ratio. Taking logarithms turns the product into $\lambda_0+\sum_i e_i$; applying the logistic inverse returns the probability. $\square$

Three consequences are operationally important.

1. Zero evidence is neutral.
2. Negative evidence must remain negative; discarding it changes the model.
3. The prior enters once. Feeding posterior probabilities that already contain the same prior into the sum double-counts it.

If a calibrated signal output $p_i$ contains a base-rate logit $\lambda_i$, its prior-free evidence is $\operatorname{logit}(p_i)-\lambda_i$. The type boundary should make this conversion explicit.

### 6.3 Robust positive-evidence pooling

Applications sometimes prefer a robust ranking rule in which a weak matching signal cannot lower a document below the pool prior. One useful family is

$$
h(p_1,\ldots,p_n)
=
\sigma\!\left(
\lambda_0+
n^\alpha
\frac{1}{n}
\sum_{i=1}^{n}
g(\operatorname{logit}(p_i))
\right),
$$

where $g$ may be softplus, ReLU, sigmoid, or a learned gate, and optional normalized weights or per-signal bounds may be added.

This family is intentionally not called exact Bayesian fusion. Gating, confidence scaling, query-pool normalization, and learned weights change the likelihood-ratio calculus. It is a bounded ranking heuristic whose usefulness is empirical. Keeping it under a distinct operator name prevents a desirable ranking policy from being mistaken for a posterior theorem.

### 6.4 Vector scores, calibration, and approximation

Cosine similarity, Euclidean distance, and index-specific scores are not probabilities. A reusable vector calibration model must identify at least the corpus, physical index, embedding model, dimensionality, candidate-pool size $K$, and model/schema version. Applying it to a different target is a model mismatch, not a harmless implementation detail.

Exact brute-force vector search and approximate IVF or HNSW search also make different claims. A calibration result does not prove approximate recall, and a recall experiment does not prove probability calibration. The physical-index contract therefore separates:

- distance or similarity correctness;
- candidate provenance and $K$;
- approximate recall against exact search;
- persistence and mutation behavior; and
- held-out reliability, expected calibration error, Brier score, and log loss.

## 7. Graph Queries as a Native Carrier

### 7.1 Property graph state

A named property graph is

$$
G=(V,E,s,t,\ell_V,\ell_E,\rho_V,\rho_E),
$$

where $V$ and $E$ are finite vertex and edge sets, $s,t:E\to V$ are endpoints, $\ell_V,\ell_E$ are labels, and $\rho_V,\rho_E$ are property maps. Named graphs may share stored entities while retaining separate membership.

Graph query results need more than document support. Define a graph payload

$$
\gamma(d)
=
(V_d,E_d,n_d,\widehat s_d),
$$

containing the matched vertex set, matched edge set, graph name, and an optional score override. A graph posting is

$$
GP=(P,\gamma)
$$

with invariant

$$
\operatorname{dom}(\gamma)
\subseteq
\operatorname{supp}(P).
$$

The invariant prevents graph metadata from referring to a result that the underlying posting does not contain.

### 7.2 Explicit graph merge policies

When two graph results overlap on document $d$, ordinary posting payloads and graph context are different contracts. The graph component uses an explicit policy

$$
\mu\in
\{\mathrm{Union},\mathrm{Intersection},
\mathrm{PreferLeft},\mathrm{PreferRight}\}.
$$

Graph-name conflicts are errors unless a chosen policy resolves them. Union and intersection of subgraph vertex and edge sets are therefore intentional graph operations, not accidental consequences of a generic map precedence rule.

### 7.3 The versioned $\Phi$ codec

Let $P_\Phi(U)\subset P_\Pi(U)$ be the decorated postings whose reserved fields contain a recognized versioned graph envelope. The codec

$$
\Phi:GP(U)\to P_\Phi(U)
$$

stores graph payloads in reserved posting fields while preserving the original score and any values that already occupied those fields.

**Theorem 7.1 (Constrained graph-posting round trip).**
For every valid graph posting $g$,

$$
\Phi^{-1}(\Phi(g))=g
$$

and

$$
\operatorname{supp}(\Phi(g))
=
\operatorname{supp}(g).
$$

**Proof sketch.** Encoding writes a versioned envelope containing the base score, exact optional score-override state, graph name, vertex and edge identifiers, and displaced reserved values. Decoding recognizes the version, restores the displaced values and base score, and reconstructs the side map under the support invariant. Encoding neither inserts nor removes posting identifiers. $\square$

This theorem is deliberately narrower than an isomorphism between arbitrary graphs and document collections. $\Phi$ is a lossless representation change for graph-posting values. It does not claim that every graph is determined by a document set or that generic posting merge is a graph-algebra homomorphism.

### 7.4 Traversal, patterns, and regular paths

A regular path expression over edge labels is generated by

$$
R ::= a \mid R_1R_2 \mid R_1\mid R_2 \mid R^*.
$$

Compilation produces an automaton $A_R=(Q,\delta,q_0,F)$. Evaluating an RPQ is reachability in the product graph

$$
G\times A_R,
$$

whose states are $(v,q)\in V\times Q$. For a fixed automaton and one start set, traversal is linear in the reachable portion of the product, bounded by

$$
O\!\left(|Q|(|V|+|E|)\right)
$$

before output materialization. Parser, NFA, and DFA size limits are physical safeguards against state explosion.

A graph pattern is better modeled as a relation of variable assignments than as an opaque “subgraph object.” If patterns $P_1$ and $P_2$ share variables, then matching their merged constraints is equivalent to a natural join of assignment relations when:

1. shared variables have the same domain and identity semantics;
2. all vertex, edge, direction, label, property, and temporal constraints are retained; and
3. multiplicity or duplicate-elimination policy is the same on both sides.

Under these conditions,

$$
\operatorname{Match}(P_1\sqcup P_2,G)
\equiv
\operatorname{Match}(P_1,G)
\bowtie
\operatorname{Match}(P_2,G).
$$

General subgraph isomorphism remains computationally hard. The presence of a unified algebra does not remove that boundary; it enables the optimizer to choose filters, indexes, traversal direction, cached patterns, or bounded RPQs while preserving semantics.

## 8. Unified Planning and Law-Indexed Optimization

### 8.1 One plan hierarchy, several execution carriers

The implementation realizes the typed framework with the plan sum

$$
\mathrm{UnifiedPlan}
=
\mathrm{Query}(\mathrm{QueryPlan})
+
\mathrm{Command}(\mathrm{CommandPlan}).
$$

A query plan owns CTEs and a relational root. Every query block carries an access decision:

$$
\mathrm{AccessPath}
=
\mathrm{Row}
+
\mathrm{OperatorTree}
+
\mathrm{Hybrid}.
$$

The row path retains SQL bag, null, scalar-expression, window, and ordering semantics. The operator-tree path handles document support, retrieval, graph, scoring, fusion, and cross-paradigm operators. The hybrid path evaluates posting candidates followed by a row-level residual. Specialized execution returns the typed sum

$$
\mathrm{OperatorOutput}
=
\mathrm{Posting}
+
\mathrm{Graph}
+
\mathrm{Generalized}.
$$

Thus “unified” does not mean that every node returns the same container. It means that every statement is lowered, optimized, and executed through one exhaustive hierarchy, and that carrier changes are explicit in that hierarchy.

The pipeline is:

$$
\mathrm{Statement}
\longrightarrow
\mathrm{UnifiedPlan}
\longrightarrow
\mathrm{plan\mbox{-}native\ optimizer}
\longrightarrow
\mathrm{UnifiedPlanExecutor}.
$$

Within a retrieval access child:

$$
\mathrm{ScalarExpr}
\longrightarrow
\mathrm{OperatorTree}
\longrightarrow
\mathrm{QueryOptimizer}
\longrightarrow
\mathrm{PlanExecutor}.
$$

Unsupported retrieval lowering returns control to the enclosing relational node. It does not invoke an independent top-level dispatcher with different semantics.

### 8.2 Law-indexed rewrite criterion

**Theorem 8.1 (Contextual rewrite soundness).**
Let $e_1,e_2:X\to C$ be pure expressions such that $e_1\equiv_C e_2$. If a context $K:C\to Y$ is deterministic and observes only $\operatorname{obs}_C$, then

$$
K[e_1]\equiv_Y K[e_2].
$$

**Proof.** For every valid input, carrier equivalence gives equal observations at the context boundary. Determinism and the observation restriction make the context outputs equal. $\square$

The theorem is elementary; its engineering consequence is substantial. A rewrite implementation must establish:

1. the carrier $C$ at the rewritten boundary;
2. the equality observed by its parent;
3. purity and error behavior;
4. the side conditions of the algebraic law; and
5. any ordering, null, multiplicity, or snapshot constraints.

The current rewrite families can be summarized as follows.

| Rewrite | Required carrier and side conditions |
| --- | --- |
| $A\wedge A\to A$, $A\vee A\to A$ | membership-only support; default payloads |
| absorption | membership-only support and structural equivalence |
| complement | explicit universe and a null-free, two-valued predicate |
| filter pushdown | deterministic predicate whose fields belong to the child; no changed null-extension |
| vector threshold merge | identical field and query vector; threshold predicate with $\max(\tau_1,\tau_2)$ |
| inner-join reordering | associative inner region; predicates preserved; no outer or lateral boundary |
| pattern-filter fusion | constraint representable inside the graph pattern |
| pattern-join fusion | compatible shared variables and identical assignment multiplicity |
| aggregate decomposition | mergeable monoid state and valid partition semantics |
| top-$k$ pushdown | exact admissible bounds and a parent for which truncation is proven safe |

This table explains why a globally enabled “Boolean simplifier” is unsafe. The implementation classifies operator-tree variants exhaustively with an is-membership-only predicate. A new variant is ineligible for Boolean idempotence until its payload effects have been reviewed.

### 8.3 Cost is not correctness

Cardinality and cost estimates select plans; they do not define query results. A simple independence estimate such as

$$
\widehat{|A\cap B|}
=
\frac{|A||B|}{|U|}\,c_{A,B}
$$

is useful only together with its model for the correlation factor $c_{A,B}$. Likewise, a graph estimate based on degree or edge-label frequency is a heuristic.

Probability bounds must state their confidence parameter. For independent Bernoulli trials with mean $\mu$, a standard multiplicative Chernoff condition is

$$
\epsilon
\ge
\sqrt{\frac{3\ln(2/\delta)}{\mu}}
$$

for the corresponding two-sided failure probability bound in the usual $0<\epsilon\le1$ regime. A bare $1/\sqrt{\mu}$ expression is not a universal high-probability guarantee.

Join enumeration uses DPccp inside reorderable inner regions and retains the selected physical strategy. Approximate vector-index recall, estimator error, and runtime latency remain empirical properties reported with corpus, parameters, executable identity, and measurement method.

## 9. Physical Execution, State, and Persistence

### 9.1 Pull execution and materialization

Relational execution uses a pull protocol over row-oriented batches. Blocking operators—sort, distinct, set operations, windows, grouping, and joins—must either account for memory and spill or document a bounded-input contract. A materialized SQL API and a streaming cursor API may expose different delivery mechanisms while preserving the same schema-ordered row observation.

The current physical row representation is dynamic and map-backed. It is not claimed to be a fully vectorized typed-column engine. Columnar result batches are produced at a boundary, and duplicate labels remain representable there, while a future positional row carrier is required to preserve two distinct in-flight values under one map key throughout execution.

### 9.2 Exact top-$k$ as a physical refinement

WAND and Block-Max WAND are physical refinements of exhaustive scoring only when they return the same ranked result. Let $S(d)$ be the complete monotone score and $B_i$ admissible upper bounds for unfinished contributions. Skipping is sound when the sum of relevant bounds cannot exceed the current $k$-th score.

For Bayesian BM25, the sigmoid is applied once after the complete raw sum. Its monotonicity permits the raw upper bound to be transformed safely. Persisted block bounds include a fingerprint of scorer parameters and field statistics; a write invalidates them. If validity changes after planning, execution falls back to exact WAND rather than using stale metadata.

### 9.3 Stateful semantics

An effectful command is modeled as

$$
f:\mathcal S\times X
\to
\mathrm{Result}(\mathcal S\times Y,E),
$$

where $\mathcal S$ includes durable catalog state, graph state, indexes, models, caches, and session-local state. For a transaction $T$, atomic visibility requires:

$$
\operatorname{publish}(T)
\text{ occurs only after every durable component of }T\text{ succeeds}.
$$

On failure, the externally visible state is observationally equivalent to the pre-transaction state. This condition is more than storage durability: in-memory graph registries, index metadata, scoring models, and caches must roll back with the catalog.

Logical sessions own transaction affinity, variables, search path, prepared plans, cancellation, sequence state, and statement serialization. Published generation counters may be shared, but mutable transaction state may not leak between sibling sessions.

### 9.4 Reopen invariance

For deterministic reads, a persistent representation is correct when closing and reopening a committed snapshot preserves the observable query result:

$$
\operatorname{obs}_C
\left(q(\operatorname{reopen}(\operatorname{persist}(\Sigma)))\right)
=
\operatorname{obs}_C(q(\Sigma)).
$$

This is an implementation refinement theorem under assumptions that serialization is complete, migrations preserve semantics, and external nondeterminism is absent. It is validated separately for documents, schemas, indexes, graphs, models, statistics, transactions, and encrypted storage.

## 10. Implementation Correspondence and Validation

### 10.1 Correspondence to UQA-RS

The UQA-RS 0.1.0 preproduction implementation maps the theory to concrete boundaries.

| Formal component | Implementation component | Enforced contract |
| --- | --- | --- |
| $D_U$ | DocSet | finite support, explicit-universe complement, Boolean property tests |
| $R_K(U)$ | Relation&lt;K&gt;, Semiring | sparse nonzero entries, pointwise plus and times |
| $P_\Pi(U)$ | PostingList, Payload | sorted unique identifiers and explicit collision policy |
| $V_s(P)$ | RankedView | descending score, deterministic identifier tie-break, separate materialization |
| $J$ | GeneralizedPostingList | lexicographically ordered identifier tuples |
| $GP(U)$ | GraphPostingList | side-map support invariant and explicit subgraph policies |
| $\Phi$ | GraphPostingCodec | versioned payload-preserving round trip |
| score domains | RawBm25Score, EvidenceLogit, PriorLogit, PosteriorProbability | explicit construction and conversion |
| typed access IR | OperatorTree, OperatorOutput | exhaustive output-carrier dispatch |
| unified statement IR | UnifiedPlan, QueryPlan, CommandPlan | exhaustive lowering and execution |
| physical refinement | TextTopKPlan, WAND, Block-Max WAND | differential equality with exhaustive scoring |

The Rust workspace separates core carriers, analysis, storage, scoring, fusion, operators, graph processing, joins, planning, physical execution, SQL, engine integration, machine learning, command-line access, APIs, protocol support, and language bindings into 21 crates. This modularity is an ownership boundary, not evidence that the runtime is fragmented: the executable plan hierarchy composes them.

### 10.2 Validation layers

The artifact uses different evidence for different kinds of claim.

1. **Algebraic properties.** Property tests exercise Boolean idempotence, commutativity, associativity, distributivity, complements, De Morgan laws, and support round trips over DocSet. Counterexample tests verify that decorated posting merges are not falsely treated as idempotent or commutative.
2. **Carrier and codec invariants.** Constructors reject graph payload keys outside support. Versioned $\Phi$ round trips preserve scores, optional overrides, graph names, vertex and edge identifiers, and pre-existing reserved fields.
3. **Optimizer equivalence.** Structural tests verify that membership-only expressions simplify while scored branches remain distinct. Randomized tests compare optimized plans with unoptimized or exhaustive execution.
4. **Top-$k$ exactness.** WAND and Block-Max WAND are compared against exhaustive ranking, including duplicate terms, field-scoped statistics, scorer changes, writes, and reopen cycles.
5. **Compatibility.** SQL and graph behavior uses golden fixtures and differential probes against the declared external semantics, including PostgreSQL-oriented cases.
6. **Persistence.** Reopen and rollback tests cover catalog objects, relational data, indexes, tensors, graphs, scoring parameters, models, views, routines, sequences, and encrypted or compressed stores.
7. **Performance methodology.** Thirty-two Rust benchmark entrypoints are tracked by a machine-checked coverage manifest. A benchmark’s presence is not itself a speed claim; published comparisons require same-machine artifacts, executable hashes, fixtures, warmup, sample count, and ratio gates.

The implementation is heavily tested but not formally verified. The value of the carrier model is that tests and review can be attached to precise laws rather than to a vague assertion of universal equivalence.

## 11. Limits and Open Problems

The framework leaves several important boundaries explicit.

1. **Finite snapshots.** The Boolean complement and completeness result is relative to a finite, fixed universe. Streaming and continuously changing universes need temporal or incremental semantics.
2. **SQL coverage and row representation.** The implementation has a broad PostgreSQL-oriented surface, not a proof of complete PostgreSQL equivalence. Its internal row carrier is dynamic rather than fully typed and vectorized.
3. **Bag and tuple boundaries.** SQL bags and generalized set-valued identifier tuples are distinct. A complete semiring treatment of every physical SQL operator remains future work.
4. **Approximate indexes.** IVF and HNSW trade exactness for recall and latency. The algebra types their results, but only differential experiments establish recall for a parameterization and corpus.
5. **Calibration.** A probability-shaped output is not evidence of calibration. Distribution shift, candidate-pool changes, and embedding-model changes require renewed held-out evaluation.
6. **Graph complexity.** General pattern matching can be exponential, and unrestricted path enumeration can produce unbounded output. Automata limits and bounded traversal are physical safeguards, not changes to complexity theory.
7. **Effects and user functions.** Volatile or externally effectful functions restrict rewrite and retry safety. Their properties must be declared and enforced at registration.
8. **Distributed execution.** Partitioned state, network failure, distributed joins, and consistency across nodes are outside the current core.
9. **Security.** Encryption and authenticated storage have separate threat models. Logical algebra does not prove confidentiality, key management, or rollback resistance.

Promising future work includes an effect system for volatility and transaction behavior, a fully positional typed row carrier, semiring annotations over generalized tuples, incremental view and graph maintenance, proof-producing rewrite registration, and distributed carrier-preserving execution.

## 12. Conclusion

Heterogeneous query execution does not require a false choice between one universal container and a collection of unrelated engines. A typed carrier algebra provides a stronger middle ground.

Document support is Boolean. Weighted relations are semiring-valued. Decorated postings preserve operational payloads without inheriting laws they do not satisfy. Ranking owns order and truncation. SQL retains bags and null semantics. Joins retain tuples. Graph results retain graph context. Probabilistic fusion distinguishes prior-free evidence from priors and separates exact inference from useful heuristics.

These carriers can still participate in one system because their operators are typed, their adapters are explicit, their rewrites are law-indexed, and their plans share one exhaustive execution hierarchy. The current UQA-RS implementation demonstrates that this is not merely a taxonomy: the distinctions determine concrete optimizer guards, score types, graph codecs, output variants, top-$k$ fallbacks, transaction boundaries, and validation tests.

The central claim is therefore substantial but precise: a single embedded runtime can compose relational, lexical, vector, graph, and probabilistic queries under one mathematical and physical planning framework, provided that unification preserves rather than erases the semantics of each carrier.

## References

1. Abiteboul, S., Hull, R., and Vianu, V. (1995). *Foundations of Databases*. Addison-Wesley.
2. Birkhoff, G. (1967). *Lattice Theory*, 3rd ed. American Mathematical Society.
3. Bonifati, A., Fletcher, G., Voigt, H., and Yakovets, N. (2018). *Querying Graphs*. Morgan & Claypool.
4. Brier, G. W. (1950). Verification of forecasts expressed in terms of probability. *Monthly Weather Review*, 78(1), 1–3.
5. Broder, A. Z., Carmel, D., Herscovici, M., Soffer, A., and Zien, J. (2003). Efficient query evaluation using a two-level retrieval process. In *Proceedings of CIKM 2003*, 426–434.
6. Codd, E. F. (1970). A relational model of data for large shared data banks. *Communications of the ACM*, 13(6), 377–387.
7. Ding, S., and Suel, T. (2011). Faster top-k document retrieval using block-max indexes. In *Proceedings of WSDM 2011*, 993–1002.
8. Graefe, G. (1994). Volcano—an extensible and parallel query evaluation system. *IEEE Transactions on Knowledge and Data Engineering*, 6(1), 120–135.
9. Gray, J., and Reuter, A. (1992). *Transaction Processing: Concepts and Techniques*. Morgan Kaufmann.
10. Green, T. J., Karvounarakis, G., and Tannen, V. (2007). Provenance semirings. In *Proceedings of PODS 2007*, 31–40.
11. Guo, C., Pleiss, G., Sun, Y., and Weinberger, K. Q. (2017). On calibration of modern neural networks. In *Proceedings of ICML 2017*, 1321–1330.
12. Libkin, L., Martens, W., and Vrgoč, D. (2016). Querying graphs with data. *Journal of the ACM*, 63(2).
13. Malkov, Y. A., and Yashunin, D. A. (2018). Efficient and robust approximate nearest neighbor search using hierarchical navigable small world graphs. *IEEE Transactions on Pattern Analysis and Machine Intelligence*, 42(4), 824–836.
14. Manning, C. D., Raghavan, P., and Schütze, H. (2008). *Introduction to Information Retrieval*. Cambridge University Press.
15. Mendelzon, A. O., and Wood, P. T. (1995). Finding regular simple paths in graph databases. *SIAM Journal on Computing*, 24(6), 1235–1258.
16. Moerkotte, G., and Neumann, T. (2006). Analysis of two existing and one new dynamic programming algorithm for the generation of optimal bushy join trees without cross products. In *Proceedings of VLDB 2006*, 930–941.
17. Robertson, S. E., and Zaragoza, H. (2009). The probabilistic relevance framework: BM25 and beyond. *Foundations and Trends in Information Retrieval*, 3(4), 333–389.
18. Jeong, J. (2025). *A Unified Mathematical Framework for Query Algebras Across Heterogeneous Data Paradigms*. OSF Preprints. https://doi.org/10.31219/osf.io/f56j2_v2
19. Jeong, J. (2025). *Extending the Unified Mathematical Framework to Support Graph Data Structures*. OSF Preprints. https://doi.org/10.31219/osf.io/cgfae_v1
20. Jeong, J. (2026). *A Unified Bayesian Framework for Hybrid Search: Calibration and Log-Odds Fusion of Lexical and Vector Retrieval*. Zenodo. https://doi.org/10.5281/zenodo.20768747
21. Jeong, J. and Cognica, Inc. (2026). UQA-RS 0.1.0 preproduction software artifact. https://github.com/cognica-io/uqa-rs
