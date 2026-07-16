# Publication Calendar 2026 — April → Day J

**Status:** v1, committed 2026-04-15.
**Parent deliberation:** [`delib-20260414-89dc`](../../.cosmon/state/fleets/default/molecules/delib-20260414-89dc/synthesis.md) — D1 tranché, D4 tranché.
**Horizon:** 2026-04-21 → Day J ≈ mid-October 2026 (J+180).
**Audience:** smallest viable audience ≈ 200 lecteurs qui cherchent activement une alternative sérieuse aux systèmes d'agents IA non-vérifiés.

## Why this calendar exists

Three thesis axes converged (C13 in the synthesis) on the same rule: editorial
rhythm must be **low, disciplined, and predictable** — not fast, not reactive,
not multi-channel. Godin provides the cadence (monthly, same day, same
channel, twelve months). Shannon provides the information budget (≤200 bits
kernel-adjacent per essay, 0 bits kernel-touching). Jobs provides the
acceptance gate (one sentence the piece must serve).

This document fixes all three so the pilot can publish without re-litigating
the format each month. Drift on cadence, bit-ceiling, or sentence-service is
the only reason to amend this file.

## The invariant sentence (Jobs gate)

> **"Un OS pour faire travailler des agents IA en parallèle sans jamais perdre
> le fil — un binaire, des fichiers, zéro serveur."**

Every essay must serve this sentence. Not rephrase it, not rebut it, not
explain it at length — *serve* it. If a draft's removal from the calendar
would leave the sentence *more* legible to a new reader, the draft is the
wrong draft. Do not publish.

The sentence appears **verbatim** as the opening or closing beat of every
essay. No variants. No translations. No "evolved" versions. Reset-narratif
cost is too high.

## Hybrid rhythm (D1 resolution)

| Axis | Rule | Source |
|------|------|--------|
| Frequency | **One essay per month, same day (second Tuesday).** First publication 2026-04-21 is the exception that anchors the series. | godin |
| Bit budget | **≤200 bits kernel-adjacent / essay; 0 bits kernel-touching.** Measured by `cs leak-audit` entropy proxy once the formula lands. Until then, eyeball + wordlist grep. | shannon |
| Subject gate | **Must serve the Jobs sentence.** If in doubt, cut or postpone. Silence is a legitimate publication. | jobs |
| Channel | **Exactly one channel per publication.** Own email list (plain text, no tracking, no lead magnet) + no-tracking RSS feed mirror. No simultaneous LinkedIn, Twitter/X, Substack, Medium, HN submission. | godin + torvalds |
| Author | **Noogram, nominative, every piece.** No pseudonym, no co-author, no guest posts, no ghostwriting. | godin + jobs |
| Format | Essay 1500–3500 words. No images that reveal internals. No animated demos. No code snippets that compile. | godin + shannon |

### How the three voices compose

- **Godin sets the clock.** Monthly. Same day. Twelve months. That creates the
  permission asset.
- **Shannon sets the ceiling.** Inside each monthly slot, the bit budget
  dictates what can be said. Kernel-adjacent topics are allowed up to 200
  bits; kernel-touching topics (TCB surface, vetoer identities, key material,
  threat-model specifics) are forbidden until freeze + audit.
- **Jobs sets the vetoing question.** Among the subjects Shannon allows, only
  those that serve the sentence get written. The result is occasional
  *silence months* — Godin's rhythm is not broken by publishing on a dead
  topic; it is broken by publishing on a topic that dilutes the sentence. If
  an essay cannot pass the Jobs gate in a given month, the pilot publishes a
  **one-paragraph cadence note** on the list ("no essay this month — see you
  on the second Tuesday of next month"), which *is* the publication for that
  slot. Cadence preserved, phrase preserved.

### Anti-patterns (forbidden by this calendar)

- **No multi-channel simultaneous publication.** Pick one channel per essay
  and own it for the full year.
- **No teaser-of-the-teaser.** One teaser (2026-04-21) for essay 1, then no
  teasers ever again.
- **No launch event.** Day J is a release, not a launch. No livestream, no
  webinar, no conference keynote before Day J.
- **No cross-essay hype loops.** Each essay stands alone. No "as I announced
  last month" / "next month I'll tell you" scaffolding.
- **No whitepaper sprint before Day J.** The Lean 4 proof object ships as
  artefact, not prose.

## Constitutional footer (invariant across all publications)

Every essay ends with the same footer, verbatim. The footer is the tribe-
building invariant: a reader who sees it twice recognizes the series; a
reader who sees it on essay 5 for the first time can navigate back to the
permission asset.

```
---

Cet essai fait partie d'une série mensuelle d'Noogram. Chaque texte
sert la même phrase : un OS pour faire travailler des agents IA en parallèle
sans jamais perdre le fil — un binaire, des fichiers, zéro serveur.

Le projet est gouverné par une Constitution écrite. Lorsque la Constitution
sera publiée, un groupe de relecteurs externes (vetoers) aura le pouvoir de
bloquer toute modification qui ne respecte pas ses invariants. Les critères
publics de recrutement de ces vetoers sont publiés séparément. L'auteur s'y
soumet par construction.

Prochain essai : <DATE>. Même canal, même heure, même signature.

— Noogram
```

**When the Constitution lands** (P3, after freeze + audit), replace
"Lorsque la Constitution sera publiée" with a direct link to the published
proof-object and its hash. **No other edits** to the footer. Ever.

## Publication gate (checklist before every essay)

Run, in order, before pressing send:

1. **Jobs gate.** Read the draft. Does it serve the sentence, verbatim-
   reconstructed, from the text alone? If no, postpone.
2. **Shannon gate.** Wordlist grep against `private-wordlist.txt` (vetoer
   names, internal tool names, TCB surface, ADR identifiers not yet public).
   Any hit = redact or cut.
3. **Entropy proxy.** Once `cs leak-audit` lands, run it; until then, eyeball
   the draft for internal acronyms, tmux references, file paths from
   `.cosmon/state/`, formulas by name. Any internal vocabulary leak = cut.
4. **Footer check.** Verbatim footer present, with correct "Prochain essai"
   date filled in. No edits to the body of the footer.
5. **Single-channel check.** Scheduled for the committed channel only. No
   cross-posts, no syndication, no "I'll just also put this on X".
6. **Constitutional hash pin.** Before send, pin the current draft of the
   Constitution (even private) via OTS. The footer's claim that a
   Constitution governs the project must be anchored by a timestamp, not by
   prose.
7. **Scrub proof.** `cs publish` (once it lands) is the single egress point.
   Until it lands: manual scrub of author metadata from the published file,
   no EXIF on accompanying images, no `~/` paths anywhere.
8. **Sign + anchor.** Sign the final published file with the pilot's Ed25519
   HW key (Yubikey). Anchor the signed-file hash via OTS. The signature +
   OTS line goes at the very end of the essay, below the footer.

Failure of any gate = postpone. A missed second-Tuesday is a one-paragraph
cadence note, not a delayed essay.

## Calendar (April → Day J)

Dates are the **second Tuesday** of each month unless an anchor date is
already committed. "Hors-slot" means the publication is off the monthly
cadence (teaser, consolidation, Day J itself).

| # | Date | Slot | Title (provisoire) | Channel | Bit budget | Tribe-building objective |
|---|------|------|--------------------|---------|------------|---------------------------|
| T0 | 2026-04-21 | hors-slot (teaser) | *Teaser essai 1* — 1 paragraphe, phrase Jobs seule | email list | ≤50 bits | Ouvrir la permission : quelques dizaines de lecteurs de cœur. |
| E1 | 2026-04-28 | hors-slot (anchor) | *Taking Gödel Seriously* — 2500 mots, principes généraux, pas d'internes | email list + RSS mirror | ≤200 bits kernel-adjacent | Établir la voix ; construire la permission list à ~200 abonnés. |
| E2 | 2026-05-12 | mensuel | *Principe gödélien dans l'orchestration* — un système ne peut être son propre témoin | email list + RSS mirror | ≤200 bits kernel-adjacent | Prouver la cadence. Lecteurs qui reviennent = noyau de la tribu. |
| E3 | 2026-06-09 | mensuel | *Pourquoi un OS d'agents mérite une Constitution* — essai-trigger du public call | email list + RSS mirror | ≤200 bits kernel-adjacent | Préparer le public call vetoer. Annonce du protocole (criteria, timeline) dans cet essai. |
| E4 | 2026-07-14 | mensuel **(option silence)** | *TBD — sujet orchestration / vetoer feedback* OU cadence-note si Jobs gate échoue | email list + RSS mirror | ≤200 bits kernel-adjacent | Maintenir la cadence sous pression ; prouver la résistance au remplissage. |
| E5 | 2026-08-11 | mensuel **(option silence renforcée)** | *TBD — premier retour vetoers public, annoncé sans nommer* OU cadence-note | email list + RSS mirror | ≤200 bits kernel-adjacent | Publier la sélection rationale du public call vetoer (sans identités, sans clés). |
| E6 | 2026-09-08 | mensuel | *Consolidation — pourquoi Day J mi-octobre* | email list + RSS mirror | ≤200 bits kernel-adjacent | Préparer Day J sans leaker la date exacte avant J-14. |
| DJ | ≈ 2026-10-13 (Day J) | hors-slot (release) | *Release coordonnée* — essai de consolidation + binaire + repo public + proof-object | email list + RSS mirror + public repo + OTS-anchored release tag | kernel-touching autorisé **uniquement** après freeze TCB + audit indépendant livrés | Transformer la tribu de lecteurs en premiers utilisateurs. |

### Per-essay notes

**T0 — 2026-04-21, teaser.** Un seul paragraphe. La phrase Jobs + promesse
d'un essai complet le mardi suivant. Aucune autre information. Anti-pattern:
pas de thread Twitter, pas de thumbnail, pas d'aperçu du sommaire.

**E1 — 2026-04-28, *Taking Gödel Seriously* (anchor).** L'essai ancre la
série. 2500 mots max. Signature nominative. Footer constitutionnel présent
(avec la formulation "Lorsque la Constitution sera publiée" puisqu'elle ne
l'est pas encore). Aucune mention du mot *noogram*, aucune mention des
régimes (Inert/Propelled/Autonomous), aucune mention du vocabulaire
physique (nucleate/evolve/collapse). Le lecteur achète une personne qui
prend Gödel au sérieux dans le cadre des systèmes d'agents ; pas une
taxonomie.

**E2 — 2026-05-12, principe gödélien dans l'orchestration.** Premier essai
sur la cadence mensuelle. Le but est de prouver que le mardi revient — pas
de produire un blockbuster. Sujet : *pourquoi un système d'agents ne peut
pas être son propre témoin*. Abstrait, applicable à Temporal, Airflow,
LangGraph, etc. Aucun exemple tiré de cosmon. Les lecteurs qui reviennent
après E1 = noyau de la tribu. C'est mesurable.

**E3 — 2026-06-09, essai-trigger vetoer public call.** Cet essai **doit**
annoncer (dernière section) que le public call pour les vetoers ouvre le
jour même de sa publication, avec lien vers
`docs/governance/vetoer-recruitment-protocol.md`. Rationale godin : trois
essais, c'est le minimum de sérieux pour qu'un senior externe accepte sans
risque réputationnel. Pas avant, pas après.

**E4 — 2026-07-14, option silence.** Premier test réel de la règle "si
l'essai ne sert pas la phrase, on ne publie pas". La pression sécurité
(fenêtre 90j post-E3, afflux de candidats vetoer à filtrer) peut rendre
difficile une contribution qui reste ≤200 bits kernel-adjacent. Si
impossible, publier une **cadence-note** d'un paragraphe. La cadence est
préservée ; le budget Shannon aussi.

**E5 — 2026-08-11, option silence renforcée.** Si E4 a été publié comme
cadence-note, E5 doit idéalement être un essai plein (deux cadence-notes
consécutives = signal négatif pour la tribu). Sujet probable : publication
de la sélection vetoer (rationale écrite pour chaque retenu, **sans
identités, sans clés, sans positions géographiques**). Si la sélection n'est
pas prête, cadence-note et on décale la publication rationale à E6.

**E6 — 2026-09-08, consolidation.** Dernier essai avant Day J. Objectif :
préparer le lecteur à Day J sans lui donner la date exacte (la date exacte
ne fuite pas avant J-14 minimum). Ton : consolidation des 5 mois précédents,
ré-ancrage de la phrase Jobs, promesse explicite que le prochain envoi sera
le Day J.

**DJ — ≈ 2026-10-13, release.** Hors calendrier mensuel. Release coordonnée
du binaire, du repo public, du proof-object Constitution (Lean 4 artefact),
et d'un essai de consolidation qui publie *pour la première fois* les
éléments kernel-touching précédemment tenus privés. Prérequis mécaniques
non-négociables (cf. synthesis P3) :

- Freeze TCB effectif.
- Audit indépendant TCB livré (rapport écrit, signé par l'auditeur).
- Proof-object Constitution Lean 4 machine-checkable sur fragment décidable.
- SLSA L2 provenance sur le release tag.
- Vetoers publics sélectionnés et en position de veto réelle (pas en
  pre-enrollment).
- Succession clause active (dead-man trigger armé).

Si un seul de ces prérequis manque à J-14, Day J se décale d'un mois. La
phrase Jobs survit à un retard ; elle ne survit pas à une release prématurée.

## Amendment rule

Cadence, bit ceiling, and Jobs gate are the three invariants of this
calendar. Any proposed amendment must:

1. Justify in writing why breaking the rule produces more tribe-building
   than silence would.
2. Pass the same gate as an essay: Jobs, Shannon, single-channel.
3. Be anchored (new version of this file, OTS timestamp) before the first
   essay affected by the amendment.

Silent drift on the monthly cadence = explicit chronicle entry required.
See an internal chronicle.
