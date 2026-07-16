---------------------------- MODULE WikiCore ----------------------------
\* POC — mechanical formalisation of the frank `wiki-core` fleet:
\*   master: /srv/cosmon/frank/.cosmon/fleet.toml             (21 lines, 5 pillars)
\*   sub:    /srv/cosmon/frank/.cosmon/fleets/wiki-core.fleet.toml (40 lines,
\*           3 STUB agents + 4 gates).
\*   kill-switch schema: ADR-0002-wiki-bis-kill-switch.md.
\*
\* Style cue: /srv/cosmon/cosmon/docs/specs/CosmonRun.tla (ADR-052 invariants).
\*
\* WHAT IS MODELLED
\*   - the 3-role linear pipeline (scope -> source -> draft) as actions that
\*     produce typed artifacts (ledger entries, articles) on bounded state.
\*   - the 5 master pillars as safety invariants on the produced artifacts:
\*       I_NamedReaderRequired, I_PrimaryPaperCited, I_NoNeologism,
\*       I_GateOrNotExist, I_AuthorNotScorer.
\*   - the 4 sub-fleet gates as a progress set gates_passed; a signed
\*     promotion decision is allowed only when all four have been collected.
\*   - the kill-switch as a boolean whose transition halts the pipeline.
\*   - monotone append-only ledger with a writer-lock (I7-like).
\*
\* WHAT IS NOT MODELLED  (honest boundary — see REPORT.md §2)
\*   - article *content* (Rice theorem: truthfulness of a string is undecidable).
\*   - LLM agent semantics (non-deterministic oracle beyond TLA+'s reach).
\*   - filesystem races outside the ledger, tmux lifecycle (out-of-band; see
\*     CosmonRun.tla for an adjacent model that DOES include those).
\*   - attestation-file frontmatter (string-level, handled by the bash
\*     tripwire in ADR-0002 §4, not by this spec).

EXTENDS Naturals, FiniteSets, Sequences, TLC

CONSTANTS
    PrimarySources,   \* finite set of required primary-source IDs
    Readers,          \* set of candidate reader names
    Authors,          \* set of candidate author agent names
    Scorers,          \* set of candidate scorer agent names (must differ from author)
    MaxArticles,      \* upper bound on number of articles (for finite model)
    NeologismPool,    \* small set of forbidden neologisms the draft MAY accidentally emit
    AllGates,         \* the four gate names (set of strings)
    NULL              \* model value for "no value" — compared by identity only

ASSUME AllGates = {"ledger-closure", "replay-green", "schema-conform", "promotion-signed"}
\* At least one (author, scorer) pair with author # scorer must exist so
\* the pipeline has a legal move; we state it existentially over the two
\* CONSTANT sets rather than insist on disjointness (they MAY overlap).
ASSUME \E a \in Authors, s \in Scorers : a # s

VARIABLES
    active_reader,       \* attested reader name, or NULL
    articles,            \* Seq of records: [slug, cites, author, scorer, neologisms]
    ledger,              \* Seq of primary-source IDs (monotone append-only)
    gates_passed,        \* SUBSET AllGates
    promotion,           \* NULL or record [verdict, signed_by]
    writer_lock,         \* NULL or "scope"; who owns ledger writes
    kill_switch          \* BOOLEAN

vars == <<active_reader, articles, ledger, gates_passed,
          promotion, writer_lock, kill_switch>>

\* ═══════════════════════════════════════════════════════════════════════
\* Derived helpers
\* ═══════════════════════════════════════════════════════════════════════

\* Range of a ledger sequence as a set.
LedgerSet == { ledger[i] : i \in 1..Len(ledger) }

\* A slug placeholder alphabet (bounded to keep the model small).
SlugPool == {"A", "B", "C"}

\* Valid article record domain (note: neologisms MAY be non-empty here —
\* the spec tracks what agents *could* produce; I_NoNeologism prunes).
ArticleRec == [ slug:        SlugPool,
                cites:       SUBSET PrimarySources,
                author:      Authors,
                scorer:      Scorers,
                neologisms:  SUBSET NeologismPool ]

\* ═══════════════════════════════════════════════════════════════════════
\* Initial state
\* ═══════════════════════════════════════════════════════════════════════

Init ==
    /\ active_reader = NULL
    /\ articles      = <<>>
    /\ ledger        = <<>>
    /\ gates_passed  = {}
    /\ promotion     = NULL
    /\ writer_lock   = NULL
    /\ kill_switch   = FALSE

\* ═══════════════════════════════════════════════════════════════════════
\* Actions
\* ═══════════════════════════════════════════════════════════════════════

\* AttestReader — ADR-0002 §3. One reader, set once.
AttestReader(r) ==
    /\ ~kill_switch
    /\ active_reader = NULL
    /\ active_reader' = r
    /\ UNCHANGED <<articles, ledger, gates_passed, promotion,
                   writer_lock, kill_switch>>

\* AcquireLedgerLock — scope agent takes the writer lock.
AcquireLedgerLock ==
    /\ ~kill_switch
    /\ writer_lock = NULL
    /\ writer_lock' = "scope"
    /\ UNCHANGED <<active_reader, articles, ledger, gates_passed,
                   promotion, kill_switch>>

\* ReleaseLedgerLock — release writer lock.
ReleaseLedgerLock ==
    /\ writer_lock = "scope"
    /\ writer_lock' = NULL
    /\ UNCHANGED <<active_reader, articles, ledger, gates_passed,
                   promotion, kill_switch>>

\* AppendToLedger — monotone append of a primary source id.
\* Requires the lock so that there is only one writer (I7-like).
AppendToLedger(src) ==
    /\ ~kill_switch
    /\ writer_lock = "scope"
    /\ src \in PrimarySources
    /\ ledger' = Append(ledger, src)
    /\ UNCHANGED <<active_reader, articles, gates_passed, promotion,
                   writer_lock, kill_switch>>

\* ProduceArticle — the draft agent writes an article.
\*
\* Every precondition corresponds to a master pillar that we intend to hold
\* as a SAFETY invariant. The guards here are the *mechanical enforcement*
\* of those pillars at production time. The invariants (below) then check
\* that the guards are collectively sufficient.
ProduceArticle(a) ==
    /\ ~kill_switch
    /\ Len(articles) < MaxArticles
    /\ active_reader # NULL                 \* pillar 1: named reader required
    /\ PrimarySources \subseteq a.cites     \* pillar 2: primary paper cited
    /\ a.neologisms = {}                    \* pillar 3: no neologism
    /\ a.author # a.scorer                  \* pillar 5: author != scorer
    /\ a.cites \subseteq LedgerSet          \* gate G_LedgerClosure
    /\ a \in ArticleRec
    /\ articles' = Append(articles, a)
    /\ UNCHANGED <<active_reader, ledger, gates_passed, promotion,
                   writer_lock, kill_switch>>

\* PassGate — each of the 4 gates becomes passed; abstracts gate script success.
PassGate(g) ==
    /\ ~kill_switch
    /\ g \in AllGates
    /\ g \notin gates_passed
    /\ (g = "ledger-closure"    => \A i \in 1..Len(articles) :
                                       articles[i].cites \subseteq LedgerSet)
    /\ (g = "promotion-signed"  => Len(articles) > 0)
    /\ gates_passed' = gates_passed \cup {g}
    /\ UNCHANGED <<active_reader, articles, ledger, promotion,
                   writer_lock, kill_switch>>

\* SignPromotion — records the promotion verdict only when the 4 gates passed.
\* Pillar 4 ("Gate or not exist") is enforced here.
SignPromotion(r) ==
    /\ ~kill_switch
    /\ promotion = NULL
    /\ gates_passed = AllGates
    /\ active_reader # NULL
    /\ r = active_reader
    /\ Len(articles) > 0
    /\ promotion' = [verdict |-> "promoted", signed_by |-> r]
    /\ UNCHANGED <<active_reader, articles, ledger, gates_passed,
                   writer_lock, kill_switch>>

\* FireKillSwitch — ADR-0002 §5. Halts further actions.
FireKillSwitch ==
    /\ ~kill_switch
    /\ kill_switch' = TRUE
    /\ UNCHANGED <<active_reader, articles, ledger, gates_passed,
                   promotion, writer_lock>>

\* ═══════════════════════════════════════════════════════════════════════
\* Next-state relation
\* ═══════════════════════════════════════════════════════════════════════

Next ==
    \/ \E r   \in Readers        : AttestReader(r)
    \/ AcquireLedgerLock
    \/ ReleaseLedgerLock
    \/ \E src \in PrimarySources : AppendToLedger(src)
    \/ \E a   \in ArticleRec     : ProduceArticle(a)
    \/ \E g   \in AllGates       : PassGate(g)
    \/ \E r   \in Readers        : SignPromotion(r)
    \/ FireKillSwitch

\* Weak fairness on promotion path so liveness properties can fire.
Spec ==
    /\ Init
    /\ [][Next]_vars
    /\ WF_vars(AcquireLedgerLock)
    /\ WF_vars(ReleaseLedgerLock)
    /\ \A g \in AllGates : WF_vars(PassGate(g))
    /\ \A r \in Readers  : WF_vars(SignPromotion(r))

\* ═══════════════════════════════════════════════════════════════════════
\* Master pillars — SAFETY invariants (5)
\* ═══════════════════════════════════════════════════════════════════════

\* Pillar 1 — articles exist only with an attested reader.
I_NamedReaderRequired ==
    (Len(articles) > 0) => (active_reader # NULL)

\* Pillar 2 — every article cites every primary source.
I_PrimaryPaperCited ==
    \A i \in 1..Len(articles) : PrimarySources \subseteq articles[i].cites

\* Pillar 3 — no article ships with neologisms.
I_NoNeologism ==
    \A i \in 1..Len(articles) : articles[i].neologisms = {}

\* Pillar 4 — promotion decision exists only if all four gates passed.
I_GateOrNotExist ==
    (promotion # NULL) => (gates_passed = AllGates)

\* Pillar 5 — author and scorer are distinct (audit-independence).
I_AuthorNotScorer ==
    \A i \in 1..Len(articles) : articles[i].author # articles[i].scorer

MasterPillars ==
    /\ I_NamedReaderRequired
    /\ I_PrimaryPaperCited
    /\ I_NoNeologism
    /\ I_GateOrNotExist
    /\ I_AuthorNotScorer

\* ═══════════════════════════════════════════════════════════════════════
\* Sub-fleet gates — SAFETY projections on state
\* ═══════════════════════════════════════════════════════════════════════

\* G_LedgerClosure — every cite in every article is in the ledger.
G_LedgerClosure ==
    \A i \in 1..Len(articles) : articles[i].cites \subseteq LedgerSet

\* G_SingleWriter — only one lock holder at a time. Structurally true by
\* writer_lock being a scalar, but stated here for the record.
G_SingleWriter ==
    writer_lock \in {NULL, "scope"}

\* G_PromotionSigned — if a promotion exists it carries a signer identity.
G_PromotionSigned ==
    (promotion # NULL) => (promotion.signed_by # NULL)

\* G_PromotionImpliesReader — signed promotion must name the attested reader.
G_PromotionImpliesReader ==
    (promotion # NULL) => (promotion.signed_by = active_reader)

SubFleetGates ==
    /\ G_LedgerClosure
    /\ G_SingleWriter
    /\ G_PromotionSigned
    /\ G_PromotionImpliesReader

\* ═══════════════════════════════════════════════════════════════════════
\* Liveness
\* ═══════════════════════════════════════════════════════════════════════

\* L_KillSwitchSticky — once fired the kill_switch never flips back.
\* Safety, phrased here as a temporal property for TLC PROPERTY.
L_KillSwitchSticky == [] (kill_switch => []kill_switch)

\* L_AttestedReaderCanPromote — with an attested reader and no kill, at
\* least one article eventually exists and the pipeline can promote.
\* This is a CONDITIONAL liveness: it says nothing when kill_switch fires
\* or when no reader ever attests.
L_AttestedReaderCanPromote ==
    \A r \in Readers :
        ((active_reader = r /\ ~kill_switch) ~> (promotion # NULL))

\* ═══════════════════════════════════════════════════════════════════════
\* TypeOK
\* ═══════════════════════════════════════════════════════════════════════

PromotionIsValid ==
    \/ promotion = NULL
    \/ /\ DOMAIN promotion = {"verdict", "signed_by"}
       /\ promotion.verdict \in {"promoted"}
       /\ promotion.signed_by \in Readers

TypeOK ==
    /\ active_reader \in Readers \cup {NULL}
    /\ articles      \in Seq(ArticleRec)
    /\ Len(articles) <= MaxArticles
    /\ ledger        \in Seq(PrimarySources)
    /\ gates_passed  \subseteq AllGates
    /\ PromotionIsValid
    /\ writer_lock   \in {NULL, "scope"}
    /\ kill_switch   \in BOOLEAN

\* ═══════════════════════════════════════════════════════════════════════
\* State constraint — keeps the model finite for TLC.
\* ═══════════════════════════════════════════════════════════════════════

StateBound ==
    /\ Len(ledger)   <= 2 * Cardinality(PrimarySources)
    /\ Len(articles) <= MaxArticles
=============================================================================
