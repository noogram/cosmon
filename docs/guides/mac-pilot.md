# mac-pilot — piloter cosmon depuis la barre de menu

`mac-pilot` est une petite app native macOS (SwiftUI + AppKit NSStatusBar)
qui vit dans la barre de menu. Depuis la v1, le popover tient **quatre
panneaux** : **Session** (le carnet de notes d'origine), **Whispers**
(l'inbox Matrix), **Inbox** (les molécules `pending` / `running`) et
**Galaxies** (la liste des galaxies sœurs).

L'icône 🧭 est toujours là, un clic suffit, une note se tape comme un SMS,
Entrée, c'est dedans. Pas de dock icon, pas de fenêtre persistante, pas de
daemon. L'app reste un front-end mince — les actions écriventes passent
par `cs`, les lectures passent directement par le filesystem.

![Barre de menu → popover mac-pilot](mac-pilot-popover.png)

> Screenshots v0 : placeholder — remplace les `.png` une fois les captures
> d'écran disponibles.

## Workflow : démarrer une session depuis la barre

1. Clic gauche sur l'icône 🧭 dans la barre de menu → le popover s'ouvre
   (< 200 ms).
2. Si aucune session n'est ouverte, clic **Start Session** (ou ⌘S).
   `mac-pilot` appelle `cs session start`, le fichier
   `/srv/cosmon/cosmon/.cosmon/state/sessions/session-<ts>.md` est créé,
   le statut en haut passe à *"Session ouverte depuis 14:32"*.
3. Le champ *Note* prend le focus automatiquement. Tape, Entrée, la note
   atterrit dans le carnet (`cs session note`). Tu peux ajouter un tag
   optionnel (ex. `insight`) dans le petit champ à gauche du bouton.
4. La liste sous le champ montre les 5 dernières notes — utile pour
   vérifier ce qu'on vient d'écrire. Rafraîchie toutes les 3 s tant que
   le popover est ouvert, en pause quand il est fermé.
5. Quand la session est terminée, clic **End Session** (ou ⌘S à nouveau).
   `cs session end` scelle le fichier avec un hash BLAKE3. Le statut
   retourne à *"Aucune session ouverte"*.

Le bouton **Terminal** en bas reste là pour les gros outils (`cs peek`,
`cs ensemble --json | jq …`) : Ghostty s'ouvre dans `/srv/cosmon/cosmon`
(fallback Terminal.app).

## Onglet Whispers — lire les messages Matrix

Bascule en ⌘2 (ou clique "Whispers" dans la barre d'onglets). Le panneau
lit directement `/srv/cosmon/cosmon/.cosmon/whispers/inbox/<room>/*.md`
toutes les 5 secondes tant que le popover est ouvert. Chaque ligne montre
`[nucleon_id] aperçu du message • il y a 3m`, ordonnée du plus récent au
plus ancien. Le badge `Whispers (N)` sur l'onglet compte les whispers
non-archivés.

Un clic sur un whisper ouvre la vue détaillée : corps complet, frontmatter
décodé (sender_mxid, event_id, origin_server_ts…), plus deux boutons.

* **Transformer en task** — shell-out `cs spark "<body>"`. Une molécule
  `idea` taggée `temp:hot` est créée et le whisper est archivé
  automatiquement dans `.cosmon/whispers/archived/<room>/`.
* **Marquer lu** — déplace seulement le `.md` vers `archived/` sans
  toucher au contenu. Le fichier est préservé à l'identique, la trace
  reste auditable.

## Onglet Inbox — les molécules en attente

⌘3 pour basculer. Le panneau shell-out `cs observe --json` une fois toutes
les 10 secondes (rate-limit volontaire : l'appel n'est pas gratuit et les
molécules bougent lentement). La liste filtre automatiquement les statuts
`pending` / `queued` / `running` / `active`. Le picker en haut propose
*Tous / temp:hot / temp:warm* ; chacun est forwardé en `--tag <glob>`. Le
badge `Inbox (N)` compte les `temp:hot`.

Chaque ligne montre `[emoji-kind] <short-id> • formula • status`. Un clic
charge le détail complet (`cs observe <id> --json`) et propose trois
actions.

* **Tackle** — `cs tackle <id>`. La molécule passe en `running`,
  un worker est lancé dans un tmux en arrière-plan. Le bouton est
  désactivé si la molécule tourne déjà.
* **Worktree** — ouvre Finder sur `.worktrees/<id>/` si le dossier existe,
  sinon sur le répertoire d'état de la molécule.
* **Collapse** — `cs collapse <id> --reason <raison>`, précédé d'un
  dialogue de confirmation qui exige une raison. Irréversible.

## Onglet Galaxies — la liste des voisines

⌘4. Le panneau scanne `/srv/cosmon/*/` une fois toutes les 30 secondes et
affiche toute sous-galaxie qui contient un dossier `.cosmon/`. Pour
chacune : nom, nombre de molécules `pending`, heure de dernière
modification. Un bouton terminal à droite lance Ghostty (ou Terminal.app)
dans la galaxie sélectionnée.

> Limitation v1 : les onglets Session / Whispers / Inbox restent
> hardcodés sur `/srv/cosmon/cosmon/`. L'onglet Galaxies n'est donc qu'un
> raccourci *pour ouvrir une voisine en terminal*, pas un switch
> applicatif. Le multi-galaxy-picker dur est prévu v2.

## Raccourcis clavier

| Touche     | Effet                                            |
|------------|--------------------------------------------------|
| ⌘1         | Onglet Session                                   |
| ⌘2         | Onglet Whispers                                  |
| ⌘3         | Onglet Inbox                                     |
| ⌘4         | Onglet Galaxies                                  |
| Entrée     | Envoyer la note en cours (onglet Session)        |
| ⌘Entrée    | Idem (backup)                                    |
| ⌘S         | Start ou End session selon l'état                |
| ⌘Q         | Quitter mac-pilot                                |
| Esc        | Fermer le popover sans envoyer                   |

## Feedback visuel

Chaque shell-out affiche un indicateur dans le coin haut-droit :

| Indicateur               | Sens                                               |
|--------------------------|----------------------------------------------------|
| Spinner (200 ms à 1 s)   | `cs` tourne, ne coupe pas le popover tout de suite |
| ✅ vert (800 ms)          | Succès — note envoyée, session scellée, etc.       |
| ⚠️ rouge (2.5 s)          | Erreur : le message détaillé s'affiche au hover    |

Codes d'erreur cosmon mappés directement :

- Exit `2` (*session already open*) → **"Une session est déjà ouverte : `<path>`. Ferme-la d'abord."**
- Exit `3` (*no open session*) → **"Aucune session ouverte. Démarre une session d'abord."**

## Installation manuelle

v0 ne notarize pas. Workflow :

1. Ouvre `apps/mac-pilot/mac-pilot.xcodeproj` dans Xcode.
2. Run (ou `xcodebuild -configuration Release build`).
3. Copie `mac-pilot.app` depuis DerivedData vers `~/Applications/` :

   ```bash
   cp -R <DerivedData>/.../mac-pilot.app ~/Applications/
   ```

4. Lance l'app ; pour qu'elle se relance au login, ajoute-la à *System
   Settings → General → Login Items*.

Voir [`apps/mac-pilot/README.md`](../../apps/mac-pilot/README.md) pour le
détail dev / build / troubleshooting.

## Scope v1 vs v2

| Fonctionnalité                  | v1 (cette PR)                           | v2 (à venir)                     |
|---------------------------------|-----------------------------------------|----------------------------------|
| `cs session start/note/end`     | ✅                                      | ✅                               |
| Whispers inbox (read/spark/ack) | ✅ — lecture FS, shell-out `cs spark`   | ✅                               |
| Molecule inbox (tackle/collapse)| ✅ — shell-out `cs observe / tackle`    | ✅                               |
| Galaxies picker (liste + open)  | ✅ — scan `/srv/cosmon/*/`               | ✅ (+ switch applicatif)         |
| Multi-galaxy picker (app state) | ❌ — hardcodé `/srv/cosmon/cosmon`       | ✅                               |
| Notarized DMG                   | ❌ — sideload manuel                    | ✅                               |
| Sandbox + entitlements          | ❌                                      | ✅ (`user-selected-files`)       |
| XPC daemon (zéro shell-out)     | ❌                                      | 👀 gated sur delib pilot post-pivot |

## Architecture (1 minute)

```
MenuBarExtra (App.swift)
        │
        ▼
PilotView (SwiftUI)
        │  @StateObject
        ▼
PilotViewModel (@MainActor)
        │  async / await
        ▼
CosmonBridge  ──►  Process("cs session …", cwd=/srv/cosmon/cosmon)
        │
        ▼
SessionParser  ◄── .cosmon/state/sessions/session-*.md
```

Le bridge parse directement le markdown `session-*.md` pour détecter la
session ouverte (celle sans `---` de fermeture), parce que `cs` n'a pas de
sous-commande `session current` v0. Quand elle arrivera, `CosmonBridge.current()`
n'aura qu'à appeler `cs --json session current` à la place.

## Pourquoi un shell-out et pas une lib cosmon embarquée ?

Parce que le CLI **est** l'interface stable. `cs session note "foo"` depuis
une app Swift doit faire exactement la même chose que `cs session note "foo"`
depuis un shell — même écriture, même commit, même seal, même reconcile.
Coller une lib Rust dans l'app, c'est dédoubler un chemin déjà stable et
casser la garantie. Le shell-out est une ligne de code (`Process.run()`)
qui hérite gratuitement de toute la logique du CLI.

Un daemon local (v1 éventuel) changerait le transport mais pas la
sémantique : même opérations, même fichiers.

## Voir aussi

- [`apps/mac-pilot/README.md`](../../apps/mac-pilot/README.md) — dev
  quickstart et troubleshooting.
- [`CHANGELOG.md`](../../CHANGELOG.md) — entrée `mac-pilot v0`.
- [`docs/guides/new-interface-faq.md`](new-interface-faq.md) — FAQ sur
  la séparation `cs session` (dictaphone) vs interfaces live (pilotage).
