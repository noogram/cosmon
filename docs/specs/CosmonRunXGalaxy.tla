-------------------------- MODULE CosmonRunXGalaxy --------------------------
\* Cross-galaxy extension of CosmonRun (ADR-052).
\*
\* Mechanical encoding of the five invariants I11..I15 sketched in the
\* "TLA-ready fragment" of delib-20260419-29f9 responses/godel.md §5,
\* named by the cross-galaxy deliberation of 2026-04-19 ("Deux cuisines,
\* deux cahiers, aucune sonnette"):
\*
\*   I11 UnionLedger                 — in-band safety
\*   I12 SingleWriterPerGalaxyField  — in-band safety
\*   I13 ContentIdentityUnderRename  — in-band safety (ADR-011 gauge)
\*   I14 PeerCompletionHonest        — OUT-OF-BAND (Gödel sentence)
\*   I15 CrossGalaxyCostBound        — in-band safety
\*
\* I14 is the cross-galaxy analogue of I9 in CosmonRun: true when every
\* galaxy is an honest peer, refuted by a TLC counterexample as soon as
\* the CONSTANT AdversarialPeerForge admits ForgePeerReceipt — a pilot
\* in galaxy g hand-forging a "Completed" receipt for peer h without h
\* actually having run the molecule. Shape identical to BypassMerge.
\*
\* B-shape (ADR-035) preserved: every cross-galaxy read walks the peer's
\* filesystem (ObservePeer); no action writes into another galaxy's state
\* tree. The super-ledger is a READ-TIME projection, never persisted.

EXTENDS CosmonRun

CONSTANTS
    Galaxies,             \* set of galaxy identifiers (e.g. {gA, gB})
    MaxCrossEdges,        \* per-molecule cap on outgoing cross-galaxy edges
    AdversarialPeerForge  \* gate: enable ForgePeerReceipt (I14 counterexample)

VARIABLES
    ledger_by_g,        \* [Galaxies -> [Mol -> 0..MaxSeqno]]   per-galaxy log
    galaxy_writer,      \* [Galaxies -> [Mol -> {"None","Owner"}]]  writer stamp
    peer_receipt,       \* [Galaxies \X Galaxies \X Mol -> {"Absent","Completed"}]
    mol_alias_epoch,    \* [Galaxies -> [Mol -> 0..MaxSeqno]]   rename counter
    cross_edges         \* [Mol -> 0..MaxCrossEdges]            edge multiplicity

xgalaxy_vars == <<ledger_by_g, galaxy_writer, peer_receipt,
                  mol_alias_epoch, cross_edges>>

full_vars == <<vars, xgalaxy_vars>>

\* ---------------- Init ----------------

XGalaxyInitVars ==
    /\ ledger_by_g      = [g \in Galaxies |-> [m \in Mol |-> 0]]
    /\ galaxy_writer    = [g \in Galaxies |-> [m \in Mol |-> "None"]]
    /\ peer_receipt     = [t \in Galaxies \X Galaxies \X Mol |-> "Absent"]
    /\ mol_alias_epoch  = [g \in Galaxies |-> [m \in Mol |-> 0]]
    /\ cross_edges      = [m \in Mol |-> 0]

XGalaxyInit == Init /\ XGalaxyInitVars

\* ---------------- In-band cross-galaxy actions ----------------

\* AppendLocalEvent — galaxy g writes to its OWN ledger and stamps itself
\* as Owner. No action in this module ever writes ledger_by_g[h] for h # g,
\* structurally witnessing I12.
AppendLocalEvent(g, m) ==
    /\ ledger_by_g[g][m] < MaxSeqno
    /\ ledger_by_g'   = [ledger_by_g   EXCEPT ![g][m] = @ + 1]
    /\ galaxy_writer' = [galaxy_writer EXCEPT ![g][m] = "Owner"]
    /\ UNCHANGED <<peer_receipt, mol_alias_epoch, cross_edges, vars>>

\* Rename — ADR-011 gauge-invariance: alias_epoch bumps; ledger and
\* writer are left untouched. Content identity is stable (I13).
Rename(g, m) ==
    /\ mol_alias_epoch[g][m] < MaxSeqno
    /\ mol_alias_epoch' = [mol_alias_epoch EXCEPT ![g][m] = @ + 1]
    /\ UNCHANGED <<ledger_by_g, galaxy_writer, peer_receipt,
                   cross_edges, vars>>

\* ObservePeer — B-shape read. Galaxy g reads peer h's events.jsonl and
\* records a witnessed receipt, but only when h's ledger already contains
\* the event. In-band Completed-claims are backed by evidence.
ObservePeer(g, h, m) ==
    /\ g # h
    /\ ledger_by_g[h][m] > 0
    /\ peer_receipt' = [peer_receipt EXCEPT ![<<g,h,m>>] = "Completed"]
    /\ UNCHANGED <<ledger_by_g, galaxy_writer, mol_alias_epoch,
                   cross_edges, vars>>

\* AddCrossEdge — records a cross-galaxy dependency, bounded by
\* MaxCrossEdges. Enforces I15: the super-ledger stays finite.
AddCrossEdge(m) ==
    /\ cross_edges[m] < MaxCrossEdges
    /\ cross_edges' = [cross_edges EXCEPT ![m] = @ + 1]
    /\ UNCHANGED <<ledger_by_g, galaxy_writer, peer_receipt,
                   mol_alias_epoch, vars>>

\* ---------------- Out-of-band adversarial action ----------------

\* ForgePeerReceipt — the Gödel adversarial action. A pilot in galaxy g
\* (with filesystem write access to its own peer_receipt table) forges a
\* "Completed" receipt naming peer h, while h's ledger is empty. The
\* signature would verify (the key lives in g's secrets). The receipt is
\* syntactically honest, semantically empty. Cross-galaxy c1cb analogue.
ForgePeerReceipt(g, h, m) ==
    /\ AdversarialPeerForge
    /\ g # h
    /\ ledger_by_g[h][m] = 0
    /\ peer_receipt' = [peer_receipt EXCEPT ![<<g,h,m>>] = "Completed"]
    /\ UNCHANGED <<ledger_by_g, galaxy_writer, mol_alias_epoch,
                   cross_edges, vars>>

\* ---------------- Next and Spec ----------------

XGalaxyStep == \E g, h \in Galaxies, m \in Mol :
    AppendLocalEvent(g, m) \/ Rename(g, m) \/ ObservePeer(g, h, m)
    \/ AddCrossEdge(m) \/ ForgePeerReceipt(g, h, m)

XGalaxyFullNext == (Next /\ UNCHANGED xgalaxy_vars) \/ XGalaxyStep

XGalaxySpec == XGalaxyInit /\ [][XGalaxyFullNext]_full_vars

\* ---------------- Invariants I11..I15 ----------------

\* I11 UnionLedger. No galaxy ever appears as a "Foreign" writer of any
\* slot: galaxy_writer[g][m] is either "None" (never written) or "Owner"
\* (written by g's own AppendLocalEvent). Foreign writes are excluded by
\* construction of the action set.
I11_UnionLedger == \A g \in Galaxies, m \in Mol :
    galaxy_writer[g][m] \in {"None","Owner"}

\* I12 SingleWriterPerGalaxyField. Every non-empty slot bears the Owner
\* stamp of its own galaxy; no galaxy writes into another's state tree.
I12_SingleWriterPerGalaxyField == \A g \in Galaxies, m \in Mol :
    (ledger_by_g[g][m] > 0) => galaxy_writer[g][m] = "Owner"

\* I13 ContentIdentityUnderRename. mol_alias_epoch lives in 0..MaxSeqno
\* independently of ledger_by_g: Rename's UNCHANGED structurally witnesses
\* that renaming cannot retroactively mutate the event log. The state
\* predicate is the type-shape anchor; the transition-level proof is the
\* Rename action itself.
I13_ContentIdentityUnderRename == \A g \in Galaxies, m \in Mol :
    /\ mol_alias_epoch[g][m] \in 0..MaxSeqno
    /\ ledger_by_g[g][m] \in 0..MaxSeqno

\* I14 PeerCompletionHonest — the Gödel sentence. Every Completed peer
\* receipt is backed by a matching entry in the peer's OWN ledger. True
\* when AdversarialPeerForge = FALSE. TLC produces a counterexample in
\* two steps when AdversarialPeerForge = TRUE, parallel to I9 / BypassMerge.
I14_PeerCompletionHonest == \A g, h \in Galaxies, m \in Mol :
    (g # h /\ peer_receipt[<<g,h,m>>] = "Completed")
        => ledger_by_g[h][m] > 0

\* I15 CrossGalaxyCostBound. The cross-galaxy edge multiplicity is
\* bounded by MaxCrossEdges per molecule. The super-ledger stays a
\* finite object; unbounded cross-references are refused at action
\* enablement.
I15_CrossGalaxyCostBound == \A m \in Mol :
    cross_edges[m] \in 0..MaxCrossEdges

\* ---------------- TypeOK ----------------

XGalaxyTypeOK ==
    /\ ledger_by_g      \in [Galaxies -> [Mol -> 0..MaxSeqno]]
    /\ galaxy_writer    \in [Galaxies -> [Mol -> {"None","Owner"}]]
    /\ peer_receipt     \in [Galaxies \X Galaxies \X Mol -> {"Absent","Completed"}]
    /\ mol_alias_epoch  \in [Galaxies -> [Mol -> 0..MaxSeqno]]
    /\ cross_edges      \in [Mol -> 0..MaxCrossEdges]

=====================================================================
