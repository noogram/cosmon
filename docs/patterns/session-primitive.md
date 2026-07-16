# Session primitive — playbook

**Status:** playbook (prose, non-normative)
**Origin:** `delib-20260420-f3ef` §10 follow-up #1 (panel knuth · torvalds · tolnay · einstein)
**Scope:** voix, showroom, HFT (when it lands). Not a crate. Not a trait.

## Thèse

La molécule cosmon **est** la primitive session. Un log ordonné single-writer,
un terminal unique, une projection recomputable, une causalité observée
avant émission — c'est tout. Chaque galaxie (voix, showroom, HFT) en
écrit sa propre variante avec son propre transport : JSONL fsynced sur
disque, SPSC ring DMA vers le thread RT, anneau shared-memory pinned.
**L'abstraction vit au niveau de l'algèbre, pas du runtime.** Pas de
crate partagé, pas de binaire générique : le runtime partagé casse
au premier ordre de grandeur de latence traversé (6 OOM entre voix
500 ms et HFT colo 100 μs). Ce que l'on partage, c'est la grammaire
des événements et les invariants qui la gouvernent ; chaque galaxie
prouve ces invariants avec les mécanismes que son budget autorise
(typestate Rust, TLA+, ou simple test-harness).

## 1. Les 4 invariants conservés

Transcrits verbatim depuis `delib-20260420-f3ef/synthesis.md §4` — les
deux autres (kill→closed, parent↔child) sont HFT-only, voir §2.

### I — Single writer on ordered log

**Forme TLA+ (template)** : `writer ∈ {None, One}` à tout instant ; un
`Append` requiert `writer = None ∨ writer = self`, et transitionne vers
`writer = self`.

**Prose** : exactement un émetteur détient le stylo. Tout le reste
observe. La race condition multi-writer est formellement interdite.

- **Voix** : `ADR-0004 §4 I5` — le bridge est seul à écrire `events.jsonl`.
- **Showroom** : `ADR-002 §2` — `&mut self` sur `EventLog`. Le
  compilateur prouve l'invariant gratuitement.
- **HFT** : un producteur pinned sur un cœur, un ring SPSC shared-memory.
  Le garant est l'ordonnanceur + l'affinity CPU, pas le type-système.

### II — Unique terminal event

**Forme TLA+ (template)** : `|{ e ∈ log : terminal?(e) }| ≤ 1`.
Renforcé par une transition `close` monotone et absorbante.

**Prose** : un session ne se termine qu'une seule fois. Les doubles
`SessionClosed` sont un bug, pas une liveness benign.

- **Voix** : `ADR-0004 §4 I1` — `SessionClosed` unique par molécule.
- **Showroom** : `ADR-002 §4` — `SongEnded` unique par setlist.
- **HFT** : deux événements terminaux candidats (`KillSwitchFired`,
  `VenueClose`) en race ; la règle de priorité est domaine-spécifique.

### III — Observe-before-emit (causality)

**Forme TLA+ (template)** : `∃ i < j : events[i] = A ∧ events[j] = B`.
Le template est partagé ; les atomes A et B sont domain-specific.

**Prose** : on observe avant de répondre. L'output n'arrive jamais avant
son input. Pas de TTS qui parle d'un outil non encore appelé.

- **Voix** : `ADR-0004 §4 I3` — `ToolCall → TTS.reply`.
- **Showroom** : `SectionEdited → BarCrossed` sur une section éditée
  pendant qu'elle joue.
- **HFT** : `OrderAck → Fill` pour un `order_id` donné.

### IV — Idempotent replay / recomputable projections

**Forme TLA+ (template)** : `project(log[1..n]) = project(log[1..n])`
après crash/restart. Les projections sont déterministes, les événements
ne sont jamais supprimés.

**Prose** : le log est la vérité ; tout le reste est vue dérivée. Tuer
le processus et relire le log doit donner exactement la même projection.

- **Voix** : `ADR-0004 §3.5` — ObservedEvent opaque, projection
  reconstructible.
- **Showroom** : `ADR-002 §3` — projections never authoritative,
  events never deleted.
- **HFT** : snapshot + tail log ; le snapshot est un cache, le log
  est le contrat.

## 2. Ce qui ne transfère PAS

Deux invariants apparentés vivent dans la délib parent, mais ne font pas
partie du noyau portable. Les nommer explicitement évite l'inflation
formelle future (on ne les réinvente pas dans showroom par symétrie
décorative).

- **Kill-switch ⟿ Closed** (liveness). HFT-only. Régulatoire
  (MiFID II RTS 6 Art. 6) : pull des ordres en vol sur panic-stop.
  Showroom ne *kill* pas un concert ; un panic-stop audio est un
  SLO (silence coupé en ≤ 5 ms), pas une transition d'état. Voix a
  l'invariant (`ADR-0004 §4 I2`) mais il est décoratif par rapport à
  HFT où il bite vraiment.
- **Parent↔child genealogy**. HFT-only dans la forme « ordre parent
  → slice VWAP enfant ». Voix nucléé des idées (`ADR-0004 §4 I4`)
  mais c'est une propriété du substrat cosmon, pas de la session
  voice elle-même. Showroom n'a aucun analogue en MVP (`ADR-003`).

Règle : si l'invariant ne s'énonce que dans une galaxie, il vit dans
son ADR local, pas dans ce playbook.

## 3. Templates TLA+ (sketches)

Prêts à spécialiser par galaxie. Les atomes domain-specific restent
en `CONSTANTS` ; la structure est partagée.

```tla
CONSTANTS Events, IsTerminal(_), IsCause(_, _)
VARIABLES log, writer

SingleWriter == writer ∈ {None} ∪ Writers
UniqueTerminal == Cardinality({ i ∈ 1..Len(log) : IsTerminal(log[i]) }) ≤ 1
ObserveBeforeEmit ==
  ∀ j ∈ 1..Len(log) :
    IsEmission(log[j]) ⇒
      ∃ i ∈ 1..j-1 : IsCause(log[i], log[j])
IdempotentReplay == project(log) = project(log)  \* tautology at spec level;
                                                  \* test-harness obligation
```

Style dérivé de `cosmon/docs/specs/CosmonRun.tla`.

## 4. Instantiations concrètes

### Voix — `voice-session.formula.toml` + JSONL fsync

- Chaque tour conversationnel nucléé une sous-molécule.
- `events.jsonl` fsync au turn-close.
- Single writer : le bridge process détient le fd ; les autres lisent.
- Terminal : `SessionClosed` écrit par le bridge en shutdown handler.
- Voir `voix/docs/adr/0004-formal-layer-atop-gradbot.md §4` pour les
  cinq invariants voix (dont quatre entrent ici).

### Showroom — SPSC ring + `EventLog` via `&mut self`

- Pas de JSONL ; le log vit en RAM + périodic snapshot.
- Thread RT produit des événements (`BarCrossed`, `NoteOn`, …) vers
  un ring SPSC ; thread main draine et projette.
- Single writer : `&mut self` sur `EventLog` prouvé par borrowck.
  Zero formal cost.
- Terminal : `SongEnded` par setlist, `&mut` garantit l'unicité.
- Voir `showroom/docs/adr/002-event-sourcing.md §2, §4` et
  `showroom/docs/adr/001-audio-backend.md §2.4` pour le budget
  p99 ≤ 15 ms.

### HFT — ring shm pinned (référence, pas de spec ici)

Pas de runtime partagé. Producteur pinned sur un cœur, consumer sur
un autre, ring shared-memory, **zéro syscall sur le hot path**.
TLA+ s'applique — c'est le domaine où les 4 invariants mordent le plus.
Architecture détaillée : voir la délib HFT séparée (follow-up #5 de
`delib-20260420-f3ef`) quand elle lande. **Ne pas réutiliser les
conclusions voice/stage pour HFT** ; le framing y diverge.

## 5. Principe directeur — honest kernel

La rigueur formelle s'ajoute là où le log franchit une frontière
durable (disque, shm, réseau) et où aucun mécanisme de langage ne la
prouve déjà. Si Rust prouve `SingleWriter` via `&mut self`, n'écrivez
pas la spec TLA+ : le compilateur le fait gratuitement. Si un
`AtomicU64` CAS garantit l'unicité terminale, la spec est documentation,
pas défense. La règle knuth : **3 invariants end-to-end + 1 template
forme-only + test-harness pour la replay**, pas 5 lignes de TLA+ par
galaxie par paresse symétrique.

Critère de révision à T+6 mois : si une galaxie a écrit une spec TLA+
qu'aucun bug n'a jamais touchée, et que le langage la prouve déjà,
archiver la spec. La dette formelle est réelle.

## 6. Références croisées

- `voix/docs/adr/0004-formal-layer-atop-gradbot.md §4` — 5 invariants
  voix (I1, I3, I4, I5 entrent dans ce playbook ; I2 reste local).
- `showroom/docs/adr/002-event-sourcing.md §2, §4` — single writer
  via `&mut self`, terminal unique, projections non authoritative.
- `showroom/docs/adr/001-audio-backend.md §2.4` — budget p99 RT.
- `cosmon/docs/specs/CosmonRun.tla` — style de référence pour les
  templates TLA+.
- `delib-20260420-f3ef/synthesis.md` — délib parent (panel knuth ·
  torvalds · tolnay · einstein).
