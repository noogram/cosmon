---------------------------- MODULE CosmonDocHarness ----------------------------
(* CosmonDocHarness — TLA+ meta-fleet skeleton for the DOC-HARNESS mission.
   Pattern source: /srv/cosmon/academy/meta-fleet/AcademyDev.tla.
   Parent deliberation: delib-20260519-a20b (synthesis Part B.4).
   Mission child: task-20260519-3cfe (C1, blocks all other implementation
   children of the parent).

   Five mission invariants encoded ; each must hold in every reachable state
   under both `CosmonDocHarness.cfg` (tight: liveness + safety) and
   `CosmonDocHarness_Safety.cfg` (widened: safety only).

     I1  NoOrphanDoc           — every PublishedDoc has a live anchor in
                                 {ADR, CRATE, CHR}. Source: jr + karpathy.
     I2  DemoGateBeforeDoc     — every adapter cited by a PublishedDoc is
                                 (a) in the published-adapter set AND
                                 (b) covered by an `adapter_selected` event
                                     plus a `step_completed` event on the
                                     same molecule.    Source: godin + karpathy + torvalds.
     I3  RegistryTruth         — PublishedAdapterNames \subseteq RegistryAdapters
                                 /\ \cap KebabRenameBait = {}
                                 /\ AdapterList line word-count <= MaxWordCount.
                                                 Source: tolnay + karpathy + torvalds.
     I4  LyapunovDecreasing    — V eventually reaches 0 (or iter caps out)
                                 under fairness on TickIteration. V counts
                                 unfinished work: open ADR/CRATE/TLA/CHR
                                 phases + draft docs + unregistered registry
                                 adapters.
     TS  TatouageShape         — in every PublishedDoc, the first mention of
                                 each tracked concept appears strictly after
                                 the first concrete demo. Source: karpathy
                                 (bonus, mechanically checkable doc-shape). *)
EXTENDS Naturals, Sequences, FiniteSets, TLC

CONSTANTS
    ArtifactIds,            \* finite set of distinct artifact identities
    Adapters,               \* finite set of adapter names (vocabulary)
    RegistryAdapters,       \* subset of Adapters known to the cosmon registry
    KebabRenameBait,        \* subset of Adapters that must NOT be published
    Concepts,               \* finite set of concept names tracked by TatouageShape
    MaxIter,                \* iteration cap (bounds the state space)
    MaxArtifacts,           \* upper bound on live artifact count
    MaxWordCount            \* upper bound on per-adapter list-line word count

ASSUME /\ RegistryAdapters  \subseteq Adapters
       /\ KebabRenameBait   \subseteq Adapters
       /\ MaxIter      \in Nat /\ MaxIter      > 0
       /\ MaxArtifacts \in Nat /\ MaxArtifacts > 0
       /\ MaxWordCount \in Nat /\ MaxWordCount > 0

----------------------------------------------------------------------------
\* Kinds and phases — modeled on AcademyDev.tla plus a DOC kind for the
\* artefacts that the doc-harness mission ships.

Kinds == {"ADR", "CRATE", "TLA", "CHR", "DOC"}

AllPhases ==
    {"DRAFT", "REVIEW", "ADOPTED", "SUPERSEDED",
     "SKETCH", "BUILDING", "MSRV_PINNED", "PUBLISHED",
     "WRITING", "RED", "GREEN",
     "CHR_DRAFT", "INSCRIBED",
     "DOC_DRAFT", "DOC_PUBLISHED"}

LegalPhases(k) ==
    CASE k = "ADR"   -> {"DRAFT", "REVIEW", "ADOPTED", "SUPERSEDED"}
      [] k = "CRATE" -> {"SKETCH", "BUILDING", "MSRV_PINNED", "PUBLISHED"}
      [] k = "TLA"   -> {"WRITING", "RED", "GREEN"}
      [] k = "CHR"   -> {"CHR_DRAFT", "INSCRIBED"}
      [] k = "DOC"   -> {"DOC_DRAFT", "DOC_PUBLISHED"}

InitialPhase(k) ==
    CASE k = "ADR"   -> "DRAFT"
      [] k = "CRATE" -> "SKETCH"
      [] k = "TLA"   -> "WRITING"
      [] k = "CHR"   -> "CHR_DRAFT"
      [] k = "DOC"   -> "DOC_DRAFT"

NULL == [missing |-> TRUE]

----------------------------------------------------------------------------
\* State.

VARIABLES
    artifacts,              \* ArtifactIds -> record (or NULL if not yet created)
    adapter_selections,     \* SUBSET (Adapters \X ArtifactIds) — adapter_selected events
    step_completions,       \* SUBSET ArtifactIds — molecules with a step_completed event
    published_adapters,     \* SUBSET Adapters — names exposed by `cs config adapters`
    adapter_word,           \* Adapters -> 0..MaxWordCount — word count of list line
    iter                    \* 0..MaxIter — global step counter (forensic clock)

vars == << artifacts, adapter_selections, step_completions,
           published_adapters, adapter_word, iter >>

LiveIds == { i \in ArtifactIds : artifacts[i] # NULL }
Art(i)  == artifacts[i]

\* Live(k, p) — IDs of live artifacts with kind k and phase p.
Live(k, p) == { i \in LiveIds : Art(i).kind = k /\ Art(i).phase = p }

PublishedDocs == Live("DOC", "DOC_PUBLISHED")
DraftDocs     == Live("DOC", "DOC_DRAFT")

\* OnDiskAnchors — the kinds + phases that count as a "live anchor" for I1.
\* CHR is anchored at INSCRIBED ; CRATE at MSRV_PINNED or later ; ADR at
\* ADOPTED or SUPERSEDED (a superseded ADR is still on disk, just frozen).
OnDiskAnchors ==
    Live("ADR", "ADOPTED")
  \cup Live("ADR", "SUPERSEDED")
  \cup Live("CRATE", "MSRV_PINNED")
  \cup Live("CRATE", "PUBLISHED")
  \cup Live("CHR", "INSCRIBED")

\* DemoEventsCover(a) — there exists an `adapter_selected` event for adapter a
\* and a `step_completed` event on the same molecule the selection targeted.
\* The synthesis predicate does NOT require temporal ordering of the two
\* events ; only that both exist with a coupled molecule identity.
DemoEventsCover(a) ==
    \E m \in ArtifactIds : << a, m >> \in adapter_selections
                        /\ m \in step_completions

----------------------------------------------------------------------------
\* Init.

Init ==
    /\ artifacts          = [ i \in ArtifactIds |-> NULL ]
    /\ adapter_selections = {}
    /\ step_completions   = {}
    /\ published_adapters = {}
    /\ adapter_word       = [ a \in Adapters |-> 0 ]
    /\ iter               = 0

----------------------------------------------------------------------------
\* Invariants.

ArtifactWellTyped == \A i \in LiveIds :
    /\ Art(i).kind  \in Kinds
    /\ Art(i).phase \in LegalPhases(Art(i).kind)

\* I1 — NoOrphanDoc.
NoOrphanDoc == \A d \in PublishedDocs :
    \E a \in Art(d).anchored_by : a \in OnDiskAnchors

\* I2 — DemoGateBeforeDoc.
DemoGateBeforeDoc == \A d \in PublishedDocs :
    \A a \in Art(d).cited_adapters :
        /\ a \in published_adapters
        /\ DemoEventsCover(a)

\* I3 — RegistryTruth.
RegistryTruth ==
    /\ published_adapters \subseteq RegistryAdapters
    /\ published_adapters \cap KebabRenameBait = {}
    /\ \A a \in published_adapters : adapter_word[a] \in 1..MaxWordCount

\* TatouageShape — for every PublishedDoc d, the doc first ran a concrete
\* demo (first_demo_iter > 0) and every tracked concept was first mentioned
\* strictly after that demo. With Concepts = {}, this is vacuously true.
TatouageShape == \A d \in PublishedDocs :
    /\ Art(d).first_demo_iter > 0
    /\ \A c \in Concepts :
         Art(d).first_concept_iter[c] > Art(d).first_demo_iter

----------------------------------------------------------------------------
\* Lyapunov function V_Doc — modeled on AcademyDev's V_Dev. Counts every
\* "open work" item: non-terminal phases for the four upstream kinds, draft
\* docs, and registry adapters not yet published. V monotonically reaches
\* 0 only if every artefact has reached its terminal phase ; under fairness
\* on TickIteration, iter caps out first and Termination holds either way.

OpenAdr   == Cardinality(Live("ADR",   "DRAFT"))
           + Cardinality(Live("ADR",   "REVIEW"))
OpenCrate == Cardinality(Live("CRATE", "SKETCH"))
           + Cardinality(Live("CRATE", "BUILDING"))
OpenTla   == Cardinality(Live("TLA",   "WRITING"))
           + Cardinality(Live("TLA",   "RED"))
OpenChr   == Cardinality(Live("CHR",   "CHR_DRAFT"))
OpenDoc   == Cardinality(DraftDocs)
OpenAdapt == Cardinality(RegistryAdapters \ published_adapters)

V_Doc == OpenAdr + OpenCrate + OpenTla + OpenChr + OpenDoc + OpenAdapt

LyapunovDecreasing == <>[](iter >= MaxIter \/ V_Doc = 0)

----------------------------------------------------------------------------
\* Actions.

SimpleArt(k, t) ==
    [ kind |-> k, phase |-> InitialPhase(k), invokes_tla |-> t,
      anchored_by |-> {}, cited_adapters |-> {},
      first_demo_iter |-> 0,
      first_concept_iter |-> [ c \in Concepts |-> 0 ] ]

DocArt(t, anchors, cited) ==
    [ kind |-> "DOC", phase |-> "DOC_DRAFT", invokes_tla |-> t,
      anchored_by |-> anchors, cited_adapters |-> cited,
      first_demo_iter |-> 0,
      first_concept_iter |-> [ c \in Concepts |-> 0 ] ]

CreateSimple(i, k, t) ==
    /\ iter < MaxIter
    /\ artifacts[i] = NULL
    /\ Cardinality(LiveIds) < MaxArtifacts
    /\ k \in {"ADR", "CRATE", "TLA", "CHR"}
    /\ artifacts' = [artifacts EXCEPT ![i] = SimpleArt(k, t)]
    /\ UNCHANGED << adapter_selections, step_completions,
                    published_adapters, adapter_word, iter >>

CreateDoc(i, t, anchors, cited) ==
    /\ iter < MaxIter
    /\ artifacts[i] = NULL
    /\ Cardinality(LiveIds) < MaxArtifacts
    /\ anchors \subseteq LiveIds
    /\ cited   \subseteq Adapters
    /\ artifacts' = [artifacts EXCEPT ![i] = DocArt(t, anchors, cited)]
    /\ UNCHANGED << adapter_selections, step_completions,
                    published_adapters, adapter_word, iter >>

AdvancePhase(i, newP) ==
    /\ artifacts[i] # NULL
    /\ Art(i).kind # "DOC"
    /\ LET k == Art(i).kind  o == Art(i).phase IN
         \/ (k = "ADR"   /\ o = "DRAFT"       /\ newP = "REVIEW")
         \/ (k = "ADR"   /\ o = "REVIEW"      /\ newP = "ADOPTED")
         \/ (k = "ADR"   /\ o = "ADOPTED"     /\ newP = "SUPERSEDED")
         \/ (k = "CRATE" /\ o = "SKETCH"      /\ newP = "BUILDING")
         \/ (k = "CRATE" /\ o = "BUILDING"    /\ newP = "MSRV_PINNED")
         \/ (k = "CRATE" /\ o = "MSRV_PINNED" /\ newP = "PUBLISHED")
         \/ (k = "TLA"   /\ o = "WRITING"     /\ newP = "RED")
         \/ (k = "TLA"   /\ o = "WRITING"     /\ newP = "GREEN")
         \/ (k = "TLA"   /\ o = "RED"         /\ newP = "GREEN")
         \/ (k = "CHR"   /\ o = "CHR_DRAFT"   /\ newP = "INSCRIBED")
    /\ artifacts' = [artifacts EXCEPT ![i] = [@ EXCEPT !.phase = newP]]
    /\ UNCHANGED << adapter_selections, step_completions,
                    published_adapters, adapter_word, iter >>

\* RegisterDemo(d, a) — forensic floor for I2. Drops an adapter_selected
\* event tied to molecule d and a step_completed event on the same molecule,
\* publishes the adapter in `cs config adapters`, and records the demo iter
\* on the doc (only on first demo). Guarded so KebabRenameBait can never
\* be published. Requires iter >= 1 to keep first_demo_iter strictly positive.
RegisterDemo(d, a) ==
    /\ iter >= 1
    /\ iter < MaxIter
    /\ artifacts[d] # NULL
    /\ Art(d).kind = "DOC"
    /\ Art(d).phase = "DOC_DRAFT"
    /\ a \in Art(d).cited_adapters
    /\ a \in RegistryAdapters
    /\ a \notin KebabRenameBait
    /\ a \notin published_adapters
    /\ adapter_selections' = adapter_selections \cup { << a, d >> }
    /\ step_completions'   = step_completions \cup { d }
    /\ published_adapters' = published_adapters \cup { a }
    /\ adapter_word'       = [ adapter_word EXCEPT ![a] = MaxWordCount ]
    /\ artifacts' =
         [ artifacts EXCEPT ![d] =
             IF Art(d).first_demo_iter = 0
                THEN [@ EXCEPT !.first_demo_iter = iter]
                ELSE @ ]
    /\ UNCHANGED << iter >>

\* MentionConcept(d, c) — record the first mention of concept c in doc d.
\* TatouageShape requires this to land strictly after the first demo, so
\* we guard `iter > first_demo_iter`. Once recorded, the field is frozen.
MentionConcept(d, c) ==
    /\ iter < MaxIter
    /\ artifacts[d] # NULL
    /\ Art(d).kind = "DOC"
    /\ Art(d).phase = "DOC_DRAFT"
    /\ Art(d).first_demo_iter > 0
    /\ iter > Art(d).first_demo_iter
    /\ c \in Concepts
    /\ Art(d).first_concept_iter[c] = 0
    /\ artifacts' = [artifacts EXCEPT ![d] = [@ EXCEPT
            !.first_concept_iter = [@ EXCEPT ![c] = iter]]]
    /\ UNCHANGED << adapter_selections, step_completions,
                    published_adapters, adapter_word, iter >>

\* PublishDoc(d) — DOC_DRAFT -> DOC_PUBLISHED. Guarded so every invariant
\* holds the moment the phase flips. Because no transition ever leaves
\* DOC_PUBLISHED, and no transition rolls back anchors / adapters once they
\* satisfy the invariants, NoOrphanDoc / DemoGateBeforeDoc / RegistryTruth /
\* TatouageShape are preserved inductively.
PublishDoc(d) ==
    /\ artifacts[d] # NULL
    /\ Art(d).kind = "DOC"
    /\ Art(d).phase = "DOC_DRAFT"
    /\ \E a \in Art(d).anchored_by : a \in OnDiskAnchors
    /\ \A a \in Art(d).cited_adapters :
         /\ a \in published_adapters
         /\ DemoEventsCover(a)
    /\ Art(d).first_demo_iter > 0
    /\ \A c \in Concepts :
         Art(d).first_concept_iter[c] > Art(d).first_demo_iter
    /\ artifacts' = [artifacts EXCEPT ![d] = [@ EXCEPT !.phase = "DOC_PUBLISHED"]]
    /\ UNCHANGED << adapter_selections, step_completions,
                    published_adapters, adapter_word, iter >>

TickIteration ==
    /\ iter < MaxIter
    /\ iter' = iter + 1
    /\ UNCHANGED << artifacts, adapter_selections, step_completions,
                    published_adapters, adapter_word >>

Next ==
    \/ \E i \in ArtifactIds, k \in {"ADR","CRATE","TLA","CHR"}, t \in BOOLEAN
         : CreateSimple(i, k, t)
    \/ \E i \in ArtifactIds, t \in BOOLEAN,
          anchors \in SUBSET ArtifactIds, cited \in SUBSET Adapters
         : CreateDoc(i, t, anchors, cited)
    \/ \E i \in ArtifactIds, p \in AllPhases : AdvancePhase(i, p)
    \/ \E d \in ArtifactIds, a \in Adapters : RegisterDemo(d, a)
    \/ \E d \in ArtifactIds, c \in Concepts : MentionConcept(d, c)
    \/ \E d \in ArtifactIds : PublishDoc(d)
    \/ TickIteration

Spec     == Init /\ [][Next]_vars /\ WF_vars(TickIteration)

\* Symmetry on ArtifactIds and Adapters — semantically valid since the
\* identities are pure tags (no per-id state lives outside `artifacts`).
\* TLC refuses symmetry on liveness properties, so the tight config
\* (which checks LyapunovDecreasing) omits SYMMETRY ; the widened
\* safety-only config uses it to compress the state space.
Symmetry == Permutations(ArtifactIds) \cup Permutations(Adapters)

----------------------------------------------------------------------------
\* Type invariant and bundled Safety predicate.

TypeOK ==
    /\ ArtifactWellTyped
    /\ iter \in 0..MaxIter
    /\ adapter_selections \subseteq (Adapters \X ArtifactIds)
    /\ step_completions   \subseteq ArtifactIds
    /\ published_adapters \subseteq Adapters
    /\ \A a \in Adapters : adapter_word[a] \in 0..MaxWordCount

Safety ==
    /\ TypeOK
    /\ NoOrphanDoc
    /\ DemoGateBeforeDoc
    /\ RegistryTruth
    /\ TatouageShape

THEOREM SpecTypeOK      == Spec => []TypeOK
THEOREM SpecSafety      == Spec => []Safety
THEOREM SpecLyapunov    == Spec => LyapunovDecreasing
============================================================================
