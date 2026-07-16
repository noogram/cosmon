# ios-pilot — guide utilisateur

**ios-pilot** est l'app native iOS/iPadOS qui transforme l'iPhone et
l'iPad en cockpit portable pour cosmon. L'opérateur tapote l'icône
🧭, navigue entre Session / Whispers / Inbox / Galaxies, déclenche une
action — le résultat atterrit dans `.cosmon/state/…` sur le MacBook Pro
à la maison, via HTTP sur [cs-api](../../crates/cosmon-api) derrière
Tailscale.

> v0 (task-20260422-b031 / -16c1) — dictaphone pour `cs session` seul.
>
> v1 (task-20260422-335b) — ajoute les onglets **Whispers**, **Inbox**,
> **Galaxies** au-dessus des endpoints `cs-api` livrés par
> task-20260422-db9f. L'Inbox et les Galaxies restent **read-only** en
> v1 (une détail pane montre la commande `cs tackle <id>` à coller sur
> le Mac). Le seul onglet qui mute est Whispers (archive + spark).

## Public cible

Le pilote de cosmon en mobilité : table café, salon, train. L'objectif
est que **plus un seul pilote n'ait besoin d'un terminal** pour
capturer une idée pendant que les workers tournent sur le Mac.

## Architecture en une image

```
iPhone / iPad (SwiftUI)
        │   HTTP (JSON) sur Tailscale WireGuard
        ▼
Mac (dev): cs-api serve --bind 0.0.0.0:4222
        │
        ▼
.cosmon/state/sessions/session-<ts>.md   (le fichier source-de-vérité)
```

Trois propriétés invariantes :

1. **Pas de shell-out depuis iOS** — l'iPhone parle HTTP.
2. **Le chiffrement est fourni par Tailscale** — WireGuard. L'app fait
   du HTTP clair au-dessus.
3. **Le Mac reste l'oracle** — les fichiers session vivent sur le Mac,
   l'iPhone est un client.

## Pré-requis sur le Mac

- `cs-api` installé (voir `task-20260422-b031`). Pendant que cs-api est
  en chantier, ios-pilot tourne sur le client mock (DEBUG only, flag
  `COSMON_USE_MOCK=1`).
- Tailscale actif, la machine dans le même tailnet que l'iPhone/iPad.
- Le daemon est lancé : `cs-api serve --bind 0.0.0.0:4222`.
- Le pare-feu macOS autorise les connexions entrantes sur 4222.

Pour connaître l'IP Tailscale du Mac :

```sh
tailscale ip -4
# ex : 100.64.0.12
```

## Build et installation

### Simulator (tests rapides)

Voir `apps/ios-pilot/README.md` pour la commande build. Puis :

```sh
xcrun simctl boot 'iPhone 17 Pro'
open -a Simulator
xcrun simctl install booted build/Debug-iphonesimulator/ios-pilot.app
xcrun simctl launch booted dev.noogram.cosmon.ios-pilot
```

### Device physique (sideload)

1. Ouvrir `apps/ios-pilot/ios-pilot.xcodeproj` dans Xcode.
2. Sélectionner le target `ios-pilot` → onglet **Signing & Capabilities**
   → choisir son équipe Apple Developer.
3. Si le bundle identifier `dev.noogram.cosmon.ios-pilot` entre en
   conflit avec un autre profil, le renommer en
   `dev.<votre-handle>.cosmon.ios-pilot` — c'est le seul changement
   nécessaire.
4. Activer le **Mode Développeur** sur l'iPhone/iPad (Réglages →
   Confidentialité et sécurité → Mode Développeur).
5. Connecter l'appareil (USB ou Wi-Fi débogage), le choisir dans le menu
   de destination Xcode, lancer **Run**.
6. La première fois, iOS demande de faire confiance au développeur :
   Réglages → Général → VPN et gestion d'appareil → profil dev.

> **Workflow complet de code signing** : on suit le même rail que
> [`blink-sideload.md`](blink-sideload.md) — la partie certificat
> dev, provisioning profile ad-hoc et trust on-device est identique.
> ios-pilot n'ajoute aucune capability spéciale (pas de push, pas de
> background refresh, pas de keychain partagé), donc le profile généré
> automatiquement par Xcode suffit.

## Configuration dans l'app

Premier lancement :

1. Ouvrir l'onglet **Réglages**.
2. Coller l'URL Tailscale du Mac : `http://100.64.0.12:4222` (remplacer
   l'IP par celle retournée par `tailscale ip -4`).
3. Taper **Tester la connexion**.
    - ✓ OK : affiche la version de `cs` et le path du binaire distant.
    - ✗ Échec : bannière rouge avec la raison (réseau, firewall, daemon
      down).
4. Laisser **Polling automatique** sur ON (défaut, intervalle 3 s).

## Utilisation quotidienne

### Ouvrir une session
Onglet **Session** → **Démarrer session** → le status en haut devient
`session-YYYY-MM-DD-HHMMSS`.

### Taper une note
Champ **Note…** est focalisé dès l'ouverture de l'app. Taper, optionnel :
choisir un tag dans le menu (`idee`, `decision`, `todo`, `spark`).
Appuyer **Envoyer** (iPhone) ou **Entrée** (iPad + clavier physique).
Haptic léger = ✓ ; haptic d'erreur = ✗.

### Consulter les dernières notes
Les 5 dernières notes de la session courante s'affichent sous le
composer. Pull-to-refresh pour forcer un sync.

### Mode hors-ligne
Si Tailscale est down ou le Mac endormi :
- Bannière rouge en haut : *"cs-api injoignable. Démarre-le sur ton Mac
  ou vérifie Tailscale."*
- Les notes tapées sont mises en file d'attente (`pending_notes` dans
  UserDefaults) et envoyées automatiquement quand la connexion revient.
- Le dot d'état en haut à droite passe rouge.

### Fermer la session
**Fermer session** → cs-api calcule le seal BLAKE3, renvoie le compte
de notes, la session disparaît du status.

## Dépannage

| Symptôme | Cause probable | Correction |
|----------|---------------|-----------|
| "cs-api injoignable" au démarrage | Mac endormi, Tailscale off, daemon pas lancé | `tailscale status` ; `cs-api serve --bind 0.0.0.0:4222` |
| "session already open" | Une session était déjà ouverte (autre client) | **Fermer session** sur l'appareil courant, ou `cs session end` côté Mac |
| Notes tapées non visibles dans le fichier Mac | cs-api écrit dans un autre galaxie | Vérifier `--galaxy` côté Mac ; les sessions sont par-galaxie |
| Polling cogne la batterie | Intervalle trop court | Dans Réglages, passer à 10–30 s, ou couper le polling |

## Les onglets v1 (nouveau dans 335b)

### Whispers

Lit `GET /whispers?limit=50` via cs-api toutes les N secondes (5 / 10 /
30, configurable dans Réglages). Chaque ligne affiche
`[sender_nucleon_id] preview… • il y a Xm`. Taper → détail pane avec
corps, frontmatter, chemin sur disque, et deux boutons :

- **Archiver** → `POST /whispers/<id>/archive`. Déplace le fichier
  `.md` vers `.cosmon/whispers/archived/<room>/`.
- **Transformer en spark** → `POST /whispers/<id>/spark`. Shell-out
  côté Mac sur `cs spark` avec le corps du whisper et le
  `sender_nucleon_id` comme nucleon. Crée une molécule `idea`.

Sur iPad, la vue passe en `NavigationSplitView` (liste à gauche, détail
à droite). Sur iPhone, navigation stack standard.

### Inbox

Lit `GET /inbox?status=pending,running` toutes les N secondes. Chaque
ligne affiche kind emoji + short-id + status badge + topic (ou formula
si vide) + tags. v1 est **read-only** — le détail pane affiche la
commande exacte `cs tackle <id>` à coller sur le Mac pour tackler. Un
bouton *Tackle* direct (POST endpoint) est v2 (une fois l'auth en
place).

Le badge de l'onglet Inbox compte les molécules taggées `temp:hot`.
Toggle **Inbox — only temp:hot** dans Réglages pour filtrer la liste
côté client.

### Galaxies

Lit `GET /galaxies` et affiche chaque galaxy (nom + pending_count +
running_count + last_activity + chemin absolu sur le Mac). v1 est aussi
**read-only** — pas de switch du galaxy actif depuis iOS. Le polling
est throttlé à 10 s mini (les galaxies changent lentement).

### Réglages (étendus v1)

Quatre sections :

1. **cs-api** — URL Tailscale + bouton *Tester la connexion* (inchangé).
2. **Rafraîchissement** — polling automatique on/off, intervalle
   **5 / 10 / 30 s** (picker discret, plus de slider continu).
3. **Filtres** — toggle *Inbox — only temp:hot*.
4. **Debug** — picker niveau de log : silencieux / erreurs / infos /
   debug. Permet de tracer les connexions cs-api dans la console Xcode.

## Ce qui n'est PAS dans v1

- **Push notifications** — polling seulement.
- **Auto-discovery du Mac** — l'URL Tailscale est collée à la main.
- **Tackle direct depuis iOS** — détail pane affiche la commande à
  coller, pas de bouton d'action (v2).
- **Switch galaxy active depuis iOS** — cs-api lit toujours
  `$HOME/galaxies/cosmon/` côté Mac (v2).
- **Chiffrement applicatif** — Tailscale fait le boulot.

## Futures itérations (v2+)

- Bonjour / mDNS pour l'auto-discovery du Mac sur le tailnet.
- Intégration neurion pour récupérer l'URL depuis le registry.
- Tackle / collapse depuis iOS (nécessite auth + endpoint POST dans cs-api).
- Switch galaxy active depuis iOS (nécessite un paramètre cs-api par requête).
- Push notifications quand un worker change d'état.
- Shared SwiftPM avec mac-pilot pour les types Models.
