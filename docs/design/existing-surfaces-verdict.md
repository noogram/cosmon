# Verdict sur les surfaces cosmon existantes — reprendre, geler, supprimer

**Molécule :** `task-20260804-51ff` (C3 du plan `idea-20260803-313f`).
**Date des mesures :** 2026-08-05. **Doc-only.**
**Aucune suppression, aucun gel n'est exécuté par cette molécule.** Ce document
est une instruction ; l'exécution demande la validation de l'opérateur.

Motif : le risque dominant du projet cockpit n'est pas technique, il est social —
*la sixième surface morte* (F1 de l'étape 1). Ajouter une sixième surface à cinq
surfaces sans verdict multiplie une dette de parité §8l. Le présent document
instruit chaque surface une par une, sur pièces.

---

## 0. Les verdicts, en une ligne chacun

| # | Surface | Verdict | Raison en une phrase |
|---|---|---|---|
| 1a | `crates/cosmon-cockpit` | **REPRENDRE** *(requalifier)* | Ce n'est pas une surface, c'est le port de lecture — `cosmon-daemon` en dépend ; le supprimer casse un crate vivant. |
| 1b | `crates/cosmon-cockpit-http` | **GELER** | Jamais installé, rien n'écoute sur son port, mais c'est le seul prototype fonctionnel de la règle d'écriture du cockpit (`POST /api/spark` → `cs nucleate`) ; le jeter avant C1 jetterait le précédent. |
| 2 | `crates/cosmon-api` / `cs-api` | **REPRENDRE** | Seule surface réellement déployée (LaunchAgent, 88 tests) — et le socle d'endpoints que le cockpit lira. Son trafic mesuré est nul, mais ce sont ses *clients* qui sont morts, pas elle. |
| 3 | `apps/mac-pilot` | **GELER** | Compile, mais 0 test, hors de tout gate CI, `.app` installé le 2026-04-23 en Debug et jamais lancé. |
| 4 | `apps/ios-pilot` | **GELER** | Compile pour le simulateur, 0 test, hors gate, aucun usage mesurable. |
| 5 | `apps/CosmonApp` | **SUPPRIMER** *(recommandé, non exécuté)* | Son serveur (`cosmon-daemon:8790`) n'écoute pas ; lecture seule, redondant avec `ios-pilot`, le moins couvert des trois apps. |
| — | `apps/CosmonKit` / `WheatPasteView` | **INVARIANT — REPRENDRE** | **Zéro consommateur mesuré.** L'outil d'enforcement de §8k′ n'est importé par aucune app. F4 n'est pas un risque futur : il est déjà réalisé. |
| — | `menubar/cosmon-pulse.10s.sh` | **REPRENDRE** *(correctif d'une ligne)* | La seule surface dans la boucle quotidienne — et elle affiche `?` toutes les 10 secondes depuis un chemin en dur inexistant. |

---

## 1. Ce qui est mesuré, ce qui est inféré, ce qui reste inconnu

### 1.1 Correction d'une prémisse de l'étape 1

L'étape 1 date l'inertie de `apps/` au 2026-07-17. Mesuré : `33d4c29`
(« cosmon v0.1.0 — initial public release ») est le **commit racine** du dépôt.
`git rev-list --max-parents=0` n'en retourne qu'un, et la première date de
`git log` est le 2026-07-17. L'histoire antérieure a été écrasée.

Conséquence sur la lecture des dates : « pas touché depuis le 2026-07-17 » ne
veut pas dire « dernière édition le 2026-07-17 », mais **« jamais touché dans
toute l'histoire visible »**. La mesure est donc *plus* dure que ce que
l'étape 1 en tirait, pas moins : en **561 commits sur 19 jours**, `apps/` n'a
reçu **aucun changement de contenu** — seulement le chore de release `v0.4.0`
du 2026-07-29, qui touche une chaîne de version.

Un second signal, indépendant du git, corrobore : le binaire
`~/Applications/mac-pilot.app/Contents/MacOS/mac-pilot` date du **2026-04-23**.

### 1.2 Table de mesures

Compilation vérifiée le 2026-08-05 sur ce poste (Swift 6.3.2, Xcode SDK iOS
simulateur présent).

| Surface | Lignes | Fichiers | Tests | Commits (hist. visible) | Compile | Déployée | Trafic mesuré |
|---|---:|---:|---:|---:|---|---|---|
| `cosmon-cockpit` | 2 030 | 5 | 37 | 5 (dernier 2026-07-26) | ✅ `cargo check` | n/a (lib) | n/a |
| `cosmon-cockpit-http` | 1 476 | 3 | 11 | 1 (racine) | ✅ `cargo check` | ❌ aucun binaire installé | rien n'écoute sur `127.0.0.1:7878` |
| `cosmon-api` / `cs-api` | 7 413 | 17 | 88 | 6 (dernier 2026-08-03) | ✅ `cargo check` | ✅ LaunchAgent, PID 832, `127.0.0.1:4222` | **0 requête tierce en 78 h** |
| `apps/mac-pilot` | 4 868 | 12 | **0** | 1 (racine) | ✅ `xcodebuild` BUILD SUCCEEDED | `.app` du 2026-04-23 dans `~/Applications` (build **Debug**) | jamais lancée (LaunchServices sans `kMDItemLastUsedDate`) |
| `apps/ios-pilot` | 4 218 | 15 | **0** | 2 (racine + chore) | ✅ `xcodebuild -sdk iphonesimulator` | ❌ | non mesurable ici |
| `apps/CosmonApp` | 1 421 | 12 | 1 fichier | 1 (racine) | ✅ `swift build` | ❌ | **son serveur `:8790` n'écoute pas** |
| `apps/CosmonKit` | 991 | 7 | 3 fichiers | 1 (racine) | ✅ `swift build` | n/a (lib) | **0 consommateur** |
| `apps/AppsTransportHTTP` | 622 | 6 | 1 fichier | — | ✅ `swift build` | n/a (lib) | 1 consommateur : `CosmonApp` |
| `menubar/cosmon-pulse.10s.sh` | 25 | 1 | 0 | 1 (racine) | n/a | ✅ symlink actif, SwiftBar PID 869 | **s'exécute et échoue toutes les 10 s** |

### 1.3 La mesure qui décide de `cs-api` : zéro requête

`cs-api` tourne en LaunchAgent (`dev.noogram.cosmon.cs-api`, `KeepAlive`,
`RunAtLoad`), démarré le 2026-08-02T09:14, soit **3 j 06 h** d'uptime au moment
de la mesure. Son log `~/Library/Logs/cosmon-cs-api.out.log` compte **6 lignes** :

- 2 lignes de démarrage — `2026-08-02T09:11:08` puis `2026-08-02T09:14:15`
  (le log est append-only à travers les redémarrages ; le processus vivant est
  celui de 09:14, ce que confirme son `ELAPSED` de `03-06:11`) ;
- 4 lignes `engine_call_entered` — **les quatre sondes émises par cette
  molécule** le 2026-08-05T15:24 (`/peek`, `/session/current`, `/ensemble`,
  `/galaxies`).

Rien d'autre. Aucune ligne entre le 2026-08-02T09:14 et le 2026-08-05T15:24.

L'argument est fermé, pas inféré : le binaire qui tourne journalise chaque
requête au niveau `INFO` (mes propres sondes l'ont prouvé sur ce même processus).
Absence de lignes = absence de requêtes. **En 78 heures, personne n'a appelé
`cs-api` sauf moi.** Le temps CPU corrobore : 0,02 s cumulées avant mes sondes,
1,18 s après.

Sondé fonctionnellement : `/peek` répond `200` en 3,3 s avec 255 341 octets ;
`/session/current`, `/ensemble`, `/galaxies` répondent `200`. La surface est
saine ; elle n'a simplement aucun appelant.

### 1.4 La mesure qui décide de `CosmonKit` : zéro consommateur

ADR-066 pose `WheatPasteView` comme **« la seule primitive SwiftUI autorisée à
afficher de l'état cosmon »** et comme *l'outil d'enforcement* de §8k′.

Mesuré, sur tout `apps/` hors `apps/CosmonKit/` lui-même :

```
grep -rn 'import CosmonKit\|WheatPasteView' apps/ --include='*.swift' \
     --include='Package.swift' --include='*.pbxproj' --include='*.yml' \
  | grep -v 'apps/CosmonKit/'
→ (aucun résultat)
```

Aucun `Package.swift`, aucun `project.yml`, aucun `project.pbxproj` ne déclare
`CosmonKit` en dépendance. `mac-pilot`, `ios-pilot` et `CosmonApp` embarquent
chacun leur propre `MarkdownView.swift` / `MarkdownTheme.swift`.

**L'outil d'enforcement n'enforce rien.** F4 de l'étape 1 — *« si deux surfaces
affichent le même état différemment sans qu'un ADR l'ait autorisé, §8k′ est mort
en silence »* — n'est pas un risque à surveiller : c'est un fait mesuré au
2026-08-05. Trois surfaces rendent l'état cosmon en vocabulaire natif SwiftUI.

Et la « remediation bead » d'ADR-066 — convertir `PilotView.swift` et
`ContentView.swift` en consommateurs de `WheatPasteView` — n'a pas été posée.
Elle ne l'a pas été **par construction** : l'ADR l'écrit noir sur blanc, *« File
at the moment the remediation is picked up; name it here only so future
reviewers recognise the pointer »*. Vérifié : aucune molécule de la flotte ne
mentionne `WheatPaste`, sauf celle-ci. §8k′ a donc été ratifié avec son outil
d'enforcement et **sans chemin d'adoption** — c'est le mécanisme exact par
lequel un invariant meurt en silence, et il est à verser au dossier de C1.

### 1.5 La mesure qui décide du pulse : la surface vivante est cassée

C'est la seule surface branchée sur la boucle quotidienne, et elle échoue.

- SwiftBar tourne (PID 869). Son `PluginDirectory` réel est
  `~/galaxies/respire/menubar` — **pas**
  `~/Library/Application Support/SwiftBar/Plugins`, dont les entrées sont des
  répertoires vides sans effet.
- Ce dossier contient `cosmon-pulse.10s.sh` → symlink (posé le 2026-06-26) vers
  `~/galaxies/cosmon/menubar/cosmon-pulse.10s.sh`.
- Le script contient, en dur : `BIN=/Users/you/.local/bin/cs`.
- `/Users/you` **n'existe pas**. Le test `[ -x "$BIN" ]` échoue, et le script
  imprime :

```
?
---
cs absent — cargo install or just install
```

`/Users/you/` est le placeholder de scrubbing du dépôt public. Il a survécu dans
un shim exécutable, pas seulement dans une documentation. Le `cs` réel est à
`~/.local/bin/cs` et `cs pulse --swiftbar` fonctionne parfaitement
quand on l'appelle directement (vérifié : sortie `🔴 DRAINAGE OFF` + les six
voyants).

Conséquence : **l'opérateur a un `?` dans sa barre de menus, et il ne l'a pas
corrigé.** C'est la mesure d'usage la plus honnête du lot — une surface qu'on
regarde vraiment ne reste pas cassée.

---

## 2. Verdict par surface

Convention utilisée, pour que les trois mots aient un sens opérationnel :

- **REPRENDRE** — du travail est budgété dessus ; elle a le droit d'apparaître
  dans une revendication de parité §8l.
- **GELER** — aucun travail de fonctionnalité ; elle reste dans le gate de
  compilation quand il existe ; **elle est retirée de toute revendication de
  parité** et son README porte la mention ; on ne la supprime pas parce qu'elle
  porte encore une information qu'on ne sait pas reconstruire à bas coût.
- **SUPPRIMER** — proposition d'exécution, soumise à l'opérateur, avec la liste
  de ce qui disparaît.

### 2.1 `crates/cosmon-cockpit` — REPRENDRE (et requalifier)

**Ce n'est pas une surface.** C'est le port hexagonal d'ADR-023 : `DashboardView`,
DTOs, `Liveness`, plus l'adapter `FileCockpitView`. Zéro I/O dans le port.

Mesuré : `crates/cosmon-daemon/Cargo.toml` en dépend, et
`crates/cosmon-daemon/src/handlers.rs` importe `cosmon_cockpit::view::{…}` et
`cosmon_cockpit::FileCockpitView` pour servir ses DTOs « verbatim, so the wire
shape stays compatible ». Supprimer ce crate casse `cosmon-daemon`.

37 tests, compile propre. Coût de reprise : nul — il n'y a rien à reprendre, il
faut le **reclasser**. Le compter parmi « les cinq surfaces » est une erreur de
catégorie qui gonfle artificiellement le décompte de surfaces mortes.

**Verdict : REPRENDRE.** Action : le nommer *port*, pas *surface*, dans les
documents qui héritent de cette énumération (dont `idea.md` §3.1).

### 2.2 `crates/cosmon-cockpit-http` — GELER

Mesuré : un seul commit (la racine) ; aucun binaire `cosmon-cockpit` installé
dans `~/.local/bin` (les `cockpit-daemon` / `cockpit-submit` qui s'y trouvent
datent du 2026-05-23 et appartiennent au cockpit *mailroom*, port 8789 — une
autre chose au même nom) ; rien n'écoute sur `127.0.0.1:7878`. 1 476 lignes,
11 tests, `cargo check` propre.

Pourquoi pas SUPPRIMER : c'est le **seul artefact existant qui implémente la
frontière d'écriture** que le projet cockpit veut instituer —
`POST /api/spark` → sous-processus `cs nucleate`, jamais une écriture directe
dans `.cosmon/state/`. C'est ADR-023 §2 rendu exécutable, avec 11 tests autour.
Le supprimer avant que C1 (`task-20260804-222e`) ait tranché la classe de
surface reviendrait à jeter le seul précédent vérifiable de la règle, pour
gagner 1 476 lignes qui ne coûtent rien : elles compilent dans le gate
workspace, sans dette de maintenance mesurée.

Pourquoi pas REPRENDRE : rien n'indique un usage, et son `static/index.html`
embarqué est exactement la forme de « sixième surface » que F1 met en garde.

**Verdict : GELER.** Action : bandeau `README`/doc-comment « gelé, non
maintenu, non revendiqué en parité §8l » ; il reste dans le workspace et donc
dans `cargo check` / `clippy` / `doc`. Réévaluation obligatoire à la sortie de
C1 : soit il devient la base de L7, soit il tombe.

### 2.3 `crates/cosmon-api` / `cs-api` — REPRENDRE

7 413 lignes, 17 fichiers, **88 tests**, 6 commits dans l'histoire visible dont
deux du 2026-08-02/03 (perf et résolution de binaires de test). C'est la seule
surface avec un chemin d'installation vivant : `scripts/install-cs-api-launchagent.sh`,
un plist documenté, un runbook (`docs/guides/launchagent-cs-api.md`), une
posture de sécurité écrite (refus de `0.0.0.0`, consentement explicite pour un
bind non-loopback).

Le trafic mesuré est nul (§1.3), et il faut lire ce chiffre correctement : les
appelants de `cs-api` sont `mac-pilot` et `ios-pilot`, gelés ci-dessous. **Zéro
requête mesure la mort des clients, pas celle du serveur.**

C'est aussi, structurellement, ce que le plan appelle déjà : *« l'instrumentation
cosmon d'abord — ce que le cockpit lira doit exister comme endpoint avant qu'un
pixel soit dessiné »*. Les 18 routes existantes sont ce socle, et elles
respectent déjà la frontière (tout en shell-out vers `cs`, aucune connaissance
du domaine côté HTTP — donc F3 tenu par construction).

**Verdict : REPRENDRE.** C'est la surface sur laquelle L2/L7 se branchent.

*Réserve à verser au dossier de C1 :* `POST /molecules/{id}/tackle` déclenche un
worker sans authentification ; le bind loopback est la seule barrière. Toute
extension du cockpit vers du pilotage distant (axe 3) rouvre cette question, et
elle n'est pas tranchée par le présent verdict.

### 2.4 `apps/mac-pilot` — GELER

Mesuré : 4 868 lignes, 12 fichiers Swift, **0 test**. `xcodebuild -scheme
mac-pilot -configuration Debug` → `** BUILD SUCCEEDED **`. Aucun workflow CI ne
compile de Swift (vérifié sur les 10 workflows de `.github/workflows/`) ; la
seule voie est `just install-mac-pilot` ou
`scripts/mac-pilot-reinstall-adhoc.sh`, tous deux manuels.

`~/Applications/mac-pilot.app` existe, daté du 2026-04-23, et c'est un build
**Debug** (`__preview.dylib`, `mac-pilot.debug.dylib` présents) — donc issu du
chemin ad-hoc, pas du chemin d'installation Release documenté.
`kMDItemLastUsedDate` est nul.

*Inféré, pas mesuré :* un `kMDItemLastUsedDate` nul est compatible avec « jamais
lancée » et avec « Spotlight n'a pas d'entrée pour ce chemin ». Le signal
converge avec le zéro-requête de `cs-api` (§1.3), dont `mac-pilot` est le
client principal : si l'app avait tourné pendant ces 78 heures, le log de
`cs-api` le montrerait. **Les deux mesures, prises ensemble, ferment le cas.**

Coût de reprise : elle compile, donc le coût n'est pas de la remettre debout —
il est de la **vérifier**. 0 test, 5 onglets, aucun gate : toute reprise
commence par écrire le harnais qui n'existe pas, ou par accepter que la seule
vérification soit un humain qui clique.

**Verdict : GELER.** Action : bandeau dans `apps/mac-pilot/README.md` ; retrait
des 4 `✅ mac-pilot …` de la colonne « Surface UI aujourd'hui » de l'audit de
parité, qui les revendique alors qu'aucune exécution n'est mesurable.

### 2.5 `apps/ios-pilot` — GELER

Mesuré : 4 218 lignes, 15 fichiers, **0 test**. `xcodebuild -sdk
iphonesimulator` → `** BUILD SUCCEEDED **`. Deux commits : la racine et le chore
de release `v0.4.0`. Cible `cs-api` (`:4222`, dont `http://100.64.0.12:4222`,
une adresse de tailnet).

Même raisonnement que `mac-pilot`, avec un facteur aggravant et un atténuant :
- aggravant : aucun usage n'est mesurable depuis ce poste (pas d'appareil), donc
  le verdict repose sur le zéro-requête de `cs-api` — ce qui suffit pour dire
  qu'aucun `ios-pilot` n'a parlé à *ce* `cs-api` en 78 h ;
- atténuant : `project.yml` (xcodegen) est présent, donc le projet est
  régénérable, ce qui n'est pas le cas de `mac-pilot`.

**Verdict : GELER.** Mêmes actions que 2.4.

### 2.6 `apps/CosmonApp` — SUPPRIMER (recommandé, non exécuté)

Mesuré : 1 421 lignes, 12 fichiers, 1 fichier de test. `swift build` → *Build
complete!*. Un seul commit (racine). Cible `cosmon-daemon` sur `:8790` via
Tailscale, en **lecture seule**.

Le fait qui la distingue des deux autres apps : **son serveur ne tourne pas.**
`lsof -nP -iTCP:8790` ne retourne rien. Le binaire `~/.local/bin/cosmon-daemon`
existe mais date du 2026-04-26 et n'est chargé par aucun job launchd
(`launchctl list | grep -i cosmon` retourne `scheduler`,
`daemon-supervisor`, `stay-awake`, `cs-api` — pas `cosmon-daemon`).

Donc : app la plus petite, la moins couverte, en lecture seule, redondante avec
`ios-pilot` sur le plan fonctionnel, et **branchée sur un endpoint éteint**. Des
trois apps Swift, c'est celle dont la suppression détruit le moins
d'information.

**Ce qui disparaîtrait si l'opérateur valide :**

- `apps/CosmonApp/` (1 421 l) — `DaemonClient`, `WireModels`, `ClusterStore`.
- `apps/AppsTransportHTTP/` (622 l) — mesuré : **`CosmonApp` est son unique
  consommateur** (`apps/CosmonApp/Package.swift` : `.package(path:
  "../AppsTransportHTTP")`). Il ne survit pas à la suppression de son seul
  appelant ; c'est un choix à faire explicitement, pas un dommage collatéral.
- La seule preuve d'usage de `cosmon-daemon` côté client. `cosmon-daemon`
  lui-même **reste** — il dépend de `cosmon-cockpit`, il compile, et il est le
  candidat naturel du pilotage distant (axe 3).

**Verdict : SUPPRIMER — recommandé, non exécuté.** Trois questions à
l'opérateur, dans cet ordre, et une seule décision à la fois :
1. supprime-t-on `CosmonApp` ?
2. si oui, supprime-t-on `AppsTransportHTTP` avec lui, ou le garde-t-on comme
   transport partagé d'un futur client ?
3. `cosmon-daemon:8790` doit-il redevenir un service chargé, ou est-ce
   `cs-api:4222` qui absorbe l'axe 3 ?

Si l'opérateur ne tranche pas, le repli sûr est **GELER**, pas supprimer : le
gel ne perd rien.

### 2.7 `apps/CosmonKit` / `WheatPasteView` — INVARIANT, pas surface

Traité à part, comme l'exige la mission. Ce n'est pas une surface : c'est
l'outil d'enforcement de §8k′ (ADR-066 §5, *« Lives at
apps/CosmonKit/Sources/WheatPasteView.swift. Every new … »*). Le supprimer
supprimerait l'enforcement du canon, pas une surface.

Mais la mesure de §1.4 change la nature de la question : **il n'enforce rien
aujourd'hui, parce que rien ne l'importe.** Un invariant dont l'outil
d'enforcement a zéro appelant est un invariant déclaratif.

**Verdict : REPRENDRE — et la reprise n'est pas « maintenir la bibliothèque »,
c'est « la câbler ou dire que §8k′ est décoratif ».** Deux issues, aucune
troisième :

- **(i)** poser la « remediation bead » qu'ADR-066 prévoit déjà — convertir au
  moins une surface non gelée en consommateur de `WheatPasteView`. Le seul
  candidat non gelé est le pulse (§2.8) ; les trois apps sont gelées, donc les
  câbler contredirait 2.4–2.6. **Conséquence honnête : si toutes les surfaces
  Swift sont gelées, §8k′ n'a plus aucun porteur, et l'invariant doit être
  reporté sur la classe de surface que C1 va définir.**
- **(ii)** l'assumer par écrit dans C1 : §8k′ ne s'applique qu'aux surfaces
  *futures*, et les surfaces gelées sont hors-champ.

Dans les deux cas, **`CosmonKit` n'est pas supprimé** : 991 lignes, 3 fichiers
de test, compile en 5,4 s, et c'est le seul endroit où le canon existe en Swift.

**C'est la conclusion la plus lourde de cette molécule, et elle est une entrée
directe de C1 (`task-20260804-222e`), pas une action de C3.**

### 2.8 `menubar/cosmon-pulse.10s.sh` — REPRENDRE

25 lignes de shim. Voir §1.5 : installée, active, et cassée par un chemin en dur
`/Users/you/.local/bin/cs` qui n'existe pas.

Coût de reprise : **une ligne** — résoudre le binaire au lieu de le coder en
dur (`command -v cs`, avec repli sur `$HOME/.local/bin/cs`). La logique réelle
est dans `crates/cosmon-cli/src/cmd/pulse.rs` et fonctionne ; le shim est le seul
défaut.

**Verdict : REPRENDRE.** C'est la surface la moins chère et la seule qui soit
déjà dans la boucle quotidienne. *Le correctif n'est pas exécuté ici* — cette
molécule est doc-only. Il est de la taille d'une molécule enfant, et il
constitue le premier test empirique de F1 : une surface d'une ligne qu'on
répare et qu'on regarde vaut plus qu'une sixième surface de 5 000 lignes.

---

## 3. Conséquence pour la dette de parité §8l

La mission pose l'hypothèse : *« une sixième surface ajoutée à cinq surfaces
sans verdict est une dette de parité §8l multipliée par six »*. La mesure la
corrige, et la correction change ce qu'il faut faire.

### 3.1 État mesuré de la dette

`docs/guides/ux-cli-parity-audit.md` **n'est pas dans le dépôt**. Il vit à
`~/galaxies/knowledge/cosmon/guides/ux-cli-parity-audit.md` (déplacé par
`task-20260714-ecbf`), dernière modification **2026-07-19**, 142 lignes.

Contenu compté à la machine :

| Mesure | Valeur |
|---|---:|
| Verbes CLI audités | **32** |
| Verbes `cs` de premier niveau aujourd'hui | **75** |
| Couverture de l'audit | **43 %** |
| Verbes avec une surface UI (`✅`) | **4** |
| Verbes partiellement couverts (`⚠️`) | **3** |
| Verbes **sans aucune surface UI** (`❌`) | **25** |
| Colonne *Reveal CLI* satisfaite | **0 / 30** |
| Colonne *Import CLI* satisfaite | **0 / 30** |

Parmi les 25 sans surface : `cs peek`, `cs health`, `cs pulse`, `cs sensorium`,
`cs ensemble`, `cs nucleate`, `cs tackle`, `cs done`, `cs collapse`,
`cs verify`, `cs wait`, `cs reconcile`. C'est-à-dire **le cœur du cycle de
vie**.

Or §8l (`docs/architectural-invariants.md` l. 1875) dit : *« Every user-facing
CLI verb has at … »*, et la ligne 1897 fait d'une ligne d'audit sans bead
`temp:warm` **une violation de §8l**.

### 3.2 La correction : la dette ne peut pas être multipliée par six

Elle ne peut pas l'être, parce qu'elle **n'est pas mesurée** :

1. **Aucun gate ne la lit.** Les 10 workflows CI ne compilent aucun Swift. Les
   sept gates du contrat projet (`cargo` ×5 + `spdx-headers.py` +
   `publish.sh --check`) ne connaissent pas `apps/`.
2. **L'audit est hors du dépôt.** Un gate de cosmon ne peut pas le lire, et
   `CLAUDE.md` comme les invariants pointent vers un chemin
   `docs/guides/ux-cli-parity-audit.md` qui n'existe plus dans l'arbre.
3. **L'audit est en dérive.** 32 verbes audités contre 75 verbes réels : 43
   verbes n'ont jamais eu de ligne, donc jamais de `❌`. La dette affichée
   (25/32) **sous-estime** la dette réelle par construction.
4. **Quatre des sept `✅`/`⚠️` sont revendiqués au nom de `mac-pilot` et
   `ios-pilot`**, dont le présent document mesure qu'elles ne s'exécutent pas.
   Après application des verdicts 2.4 et 2.5, la colonne « Surface UI
   aujourd'hui » retombe à **0 ✅** — la dette n'est pas multipliée par six,
   **elle est de 100 % et l'était déjà.**

**La conclusion opérationnelle est donc l'inverse de l'intuition de départ.** Le
danger n'est pas d'ajouter une sixième colonne à un tableau tenu ; c'est que le
tableau n'est tenu par rien. Une sixième surface n'aggraverait pas une dette
mesurée : elle hériterait d'une **absence de mesure**, et c'est précisément
ainsi que les cinq premières sont mortes sans que personne le voie.

### 3.3 Ce que cela impose au reste du plan

1. **C4 (`task-20260804-2bbb`, canon de surface) n'est pas optionnel : c'est la
   précondition d'une sixième surface.** Le précédent invoqué par le plan —
   `crates/cosmon-rpp-adapter/data/surface_events.txt` parsé par
   `cosmon-surface-canon` en build-dependency, qui casse le build si une route
   n'est pas déclarée — est exactement le mécanisme qui manque à §8l. La
   différence entre les cinq surfaces mortes et une sixième vivante n'est pas
   la techno d'UI : c'est l'existence d'un gate qui échoue quand la parité
   diverge.
2. **§8l doit être re-domicilié ou remplacé, et c'est une question de C1.** Un
   invariant dont l'audit vit hors du dépôt et couvre 43 % du CLI ne peut pas
   être gaté. Les deux issues sont : rapatrier l'audit et le rendre
   machine-vérifiable contre `cs --help` (75 lignes, pas 32), ou le remplacer
   par le canon de C4. La seconde est cohérente avec la contrainte opérateur
   « projet séparé » ; la première ne l'est pas.
3. **Après application des verdicts, il ne reste pas cinq surfaces mais deux
   vivantes** — `cs-api` (reprise) et `cosmon-pulse` (reprise, à réparer) —
   plus un port (`cosmon-cockpit`), trois gels et une proposition de
   suppression. Le cockpit ne sera pas la sixième surface : il sera la
   **troisième**, et les deux autres sont celles qu'on mesure.

---

## 4. Ce qui reste inconnu

Nommé plutôt que comblé :

1. **`ios-pilot` et `CosmonApp` sur appareil.** Aucun iPhone/iPad n'est
   interrogeable depuis ce poste. Le verdict de gel repose sur le zéro-requête
   de `cs-api` (valide pour `ios-pilot`) et sur l'extinction de `:8790` (valide
   pour `CosmonApp`) — pas sur une observation de l'appareil.
2. **`kMDItemLastUsedDate` nul.** Compatible avec « jamais lancée » et avec
   « non indexée ». §2.4 explique pourquoi le faisceau ferme quand même.
3. **L'histoire antérieure au 2026-07-17.** Écrasée par le commit racine.
   L'ancienneté réelle du code des apps n'est pas datable depuis ce dépôt ; le
   binaire du 2026-04-23 est le seul repère.
4. **Ce que l'opérateur voulait faire de `mac-pilot`.** Le gel est réversible et
   c'est pourquoi il est proposé plutôt qu'une suppression : la question
   « morte ou en pause » (inconnue n° 3 de l'étape 1) reste ouverte pour les
   deux pilotes, et le gel est exactement la réponse qui n'exige pas d'y
   répondre.

---

## 5. Critère de sortie

Une ligne de verdict par surface, avec sa raison écrite : §0 et §2. Chaque
verdict adossé à une mesure et non à une supposition : §1. Conséquence pour la
dette de parité §8l : §3. `CosmonKit`/`WheatPasteView` traité comme invariant et
non comme surface : §2.7. **Aucune suppression, aucun gel exécuté.**
