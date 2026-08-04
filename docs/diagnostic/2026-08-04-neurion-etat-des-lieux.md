# Point d'étape Neurion — état mesuré, verdict, recommandation

**Molécule** : `task-20260803-2679` — analyse, aucun développement.
**Demande opérateur** : 2026-08-03. Contexte : Neurion est candidat comme socle
de notarisation/archivage des sessions de pilotage (axe 4 de `idea-20260803-313f`,
« cockpit cosmon »).
**Date de la mesure** : 2026-08-04.

---

## 0. Le résumé en cinq lignes

Neurion est un **annuaire local de l'environnement** : une base SQLite qui répond
à « où vit le mail, et comment je le cherche ? ». Cette partie-là marche et sert
vraiment, mais elle est petite : 17 domaines, 41 chemins d'accès, écrits à la main.
Autour, il y a trois étages de modèle (hypergraphe, dérive de galaxies, surfaces
de Shannon) qui n'ont **jamais été appelés une seule fois**. Et le registre est
aujourd'hui rempli à 99,2 % de répertoires temporaires produits par la suite de
tests de cosmon, qui grossit d'environ mille lignes par jour.

Verdict court : **utile mais banal dans sa catégorie, sur-conçu dans sa moitié
haute, et en panne d'entretien depuis six semaines**. Comme socle de notarisation :
**le motif oui, l'instance non**.

---

## 1. Ce qui tourne (mesuré, pas inféré)

### 1.1 Où vit le code

| Emplacement | Rôle | Volume |
|---|---|---|
| `~/galaxies/noogram-distro/plugins/mcp/crates/{neurion-core,neurion-mcp}` | source vivante | ~5 800 lignes Rust |
| `~/galaxies/cosmon/crates/neurion-core` | copie vendorisée (ADR-126 §6 classe 2) | 1 826 lignes |
| `~/dev/ESERIE/.archive/neurion-2026-04-16` | dépôt d'origine, archivé | 5 556 lignes, 29 commits |
| `~/.local/bin/neurion` | **binaire déployé**, daté du 16 juin 2026 | 3,8 Mo |
| `~/Library/Application Support/neurion/neurion.db` | base vivante | 6,4 Mo + 4,2 Mo de WAL |

Le binaire déployé est à jour fonctionnellement : la résolution de fiche personne
(ADR-005, `PersonSurface`) répond correctement en sondage direct — donc il porte
au moins le code d'avril tardif.

### 1.2 Ce que la base contient

```
referents          17     reaches            41     surfaces            5
chronicles          4     edges              18     surface_signals     0
mcp_servers        16     services           44     agents             47
binaries           64     config_files       24     databases           4
organizations       3     nodes          21 484     repos          21 486
```

`repos` — la plus grosse table — se décompose ainsi :

| Origine | Lignes |
|---|---|
| Vrais dépôts (`/Users/...`) | **164** |
| Répertoires temporaires (`/private/var/folders`, `/private/tmp`) | **21 322** |

Et ça continue : 506 lignes temporaires ajoutées le 4 août à 16 h, 1 342 le 3 août,
1 764 le 28 juillet. Rythme d'environ **1 000 lignes de déchet par jour**.

**Cause mesurée.** `cosmon-cli` émet un « hint » d'auto-enregistrement quand `cs`
tourne dans un dépôt (`crates/cosmon-cli/src/neurion_hint.rs`), et `neurion-mcp`
draine ce fichier au démarrage. Le garde-fou `is_excluded_repo_path` exclut
`.worktrees` et `.git` — **mais pas les répertoires temporaires**. Les tests de
cosmon qui font `cs nucleate` dans un `tempdir` n'ont de protection que la variable
d'environnement `NEURION_AUTO_REGISTER_FILE`, et cette variable n'est posée que
dans une poignée de fichiers de test. Tous les autres écrivent dans le vrai
registre de l'opérateur.

C'est l'image d'un carnet d'adresses où, chaque fois qu'on répète une pièce de
théâtre, les noms des figurants sont recopiés en dur — et jamais effacés.

### 1.3 La conséquence directe : un outil qui peut noyer une session

`list_services(category: "repo")` exécute `SELECT ... FROM repos` **sans `LIMIT`**
(`crates/neurion-mcp/src/adapter/sqlite.rs:521`). La charge utile actuelle de cette
seule requête pèse **2,96 Mo**, soit de l'ordre de 800 000 tokens. Aucun modèle ne
l'avale. `category: "all"` est pire (21 688 lignes).

Ce n'est pas théorique : c'est l'outil que le mode d'emploi MCP de neurion
recommande explicitement en deuxième ligne. Il a été appelé **une seule fois** en
5 788 sessions.

Deuxième défaut du même genre, plus discret : `how_to_access` colle
systématiquement, sous la réponse sémantique, un `LIKE '%mot%'` non borné sur les
descriptions de huit tables (« Also found in inventory »). Sur `how_to_access("email")`,
la partie utile fait environ 350 tokens ; la queue de bruit qui suit en fait
environ 2 500 — des pages Cloudflare qui matchent parce que leur description
contient « emails: ». **Un rapport signal/bruit d'environ 1 pour 7 sur l'outil
phare.**

---

## 2. Ce qui est utilisé

Comptage sur 5 788 sessions Claude Code archivées (blocs `tool_use` réels, pas les
définitions d'outils injectées dans le prompt système) :

| Outil | Appels | Verdict |
|---|---|---|
| `query_registry` (SQL brut) | 127 | vivant |
| `how_to_access` | 90 | vivant |
| `upsert_entry` | 53 | vivant |
| `get_config_for` | 2 | marginal |
| `list_services` | 1 | mort-né |
| `add_node` / `add_edge` / `query_graph` / `describe_node` / `delete_entry` | **0** | mort |

**273 appels au total, dans 67 sessions** (1,2 % des sessions). Répartition dans le
temps : 19 en mai, 46 en juin, 208 en juillet, **0 en août** (4 jours). Galaxies
utilisatrices : secretariat (7 sessions), cosmon (7), maqi (6), stagecraft (3),
puis une longue traîne.

Fait notable : l'usage réel n'est pas le graphe ni le classement d'intentions,
c'est **le SQL brut** (127 appels). Les agents contournent l'API sémantique et
interrogent les tables directement. Quand la couche « intelligente » est
court-circuitée par ses propres utilisateurs, c'est une donnée sur sa valeur.

**Usage indirect, lui bien réel.** cosmon dépend de `neurion-core` pour trois
choses qui, elles, tournent à chaque commande : `GalaxyKind` (utilisé par
`cs galaxies`, `cs mur`, `cs status`), `schema::SCHEMA_SQL` (utilisé par `cs init`
et `cs doctor`), et les hints d'auto-enregistrement. C'est ce qui fait que neurion
n'est pas supprimable d'un trait de plume aujourd'hui.

---

## 3. Ce qui est mort

| Chantier | Volume | Preuve de mort |
|---|---|---|
| Hypergraphe (nodes/edges/edge_endpoints, ADR-004) | 4 outils MCP + schéma dédié | **0 appel** ; 18 arêtes, toutes écrites par le seeding |
| Surfaces à paramètres de Shannon (`H_s`, `λ_s`, `I_s`, `D_s`, capacité) | table + colonnes | `surface_signals` = **0 ligne** ; 5 surfaces, dernier signal le 30 mai |
| Détection de dérive de galaxies (5 fonctions) | 547 lignes + 11 tests + 5 doctests | **0 appelant** dans tout `~/galaxies` ; la « formule qui nucléerait une molécule `issue` » promise en doc n'existe pas |
| Trait `Reachable` | 31 lignes, marqué `#[allow(dead_code)]` | **0 implémenteur**, alors que la THESIS le présente comme « l'abstraction partagée entre Neurion, OxyMake et Cosmon » |
| Index chronique inter-galaxies | table `chronicles` | **4 lignes indexées** contre **890 entrées** réelles dans 65 fichiers `CHRONICLES.md` — soit 0,45 % de couverture ; dernière indexation le 10 mai |
| Feature `neurion-fallback` de `cosmon-registry` | dépendance optionnelle | activée **nulle part** |

Ordre de grandeur, en ne comptant que ce que j'ai pu attribuer à un fichier :
`drift.rs` seul fait 547 lignes sur les ~5 800 du projet, et la couche hypergraphe
(schéma + adaptateur + 4 outils) plus les colonnes de surfaces s'y ajoutent sans
que je puisse en donner un chiffre net. **Au bas mot un cinquième du code de
neurion est un modèle joliment documenté que personne n'a jamais exercé.** Le cas
`Reachable` est le plus parlant — un trait
dont le doc-comment énumère trois implémentations, et qui en a zéro.

---

## 4. Le développement a-t-il fonctionné ? (rétrospective)

### 4.1 Ce qui a bien marché

- **Sprint initial dense et propre.** 29 commits du 5 au 14 avril, avec ADRs,
  THESIS, TUTORIAL, glossaire de langue ubiquitaire. Le dépôt d'avril portait
  **129 fonctions de test**.
- **L'hexagone tient vraiment.** J'ai vérifié le point où ce genre de promesse
  casse d'habitude : le scoring. `default_score`, `intent_score` et
  `compute_health` vivent bien dans le crate `-core` sans I/O, et l'adaptateur
  SQLite les appelle (`sqlite.rs:833`, `:845`, `:1058`). La logique n'a pas fui
  dans le SQL. C'est rare et ça mérite d'être dit.
- **Les portes passent.** `cargo test --workspace` sur le plugin : **99 tests,
  0 échec**, en isolation du noyau cosmon, exactement comme la THESIS le promet.

### 4.2 Ce qui n'a pas marché

**a) La suite de tests a été amputée à la migration, sans que rien ne vire au rouge.**

| | avril (dépôt d'origine) | aujourd'hui (plugin) |
|---|---|---|
| Fonctions `#[test]` neurion, total | 129 | 47 |
| dont modules existant déjà en avril | 129 | **29** |
| dont `tests.rs` (intégration) | 111 | **13** |
| `src/domain/resolver.rs` | 10 tests | **fichier disparu** |
| dont modules créés *après* avril (`drift`, `galaxy_kind`) | — | 18 |

Sur le périmètre comparable — les modules qui existaient déjà en avril —
**129 tests sont devenus 29, soit 78 % de la couverture perdue** ; la suite
d'intégration seule passe de 111 à 13. Les 18 tests qui restent au total sont
ceux de modules ajoutés depuis, qui masquent l'amputation dans le compte global.
Personne ne l'a vu, parce qu'une suite amputée est verte. C'est exactement le motif « ne jamais faire passer une porte au vert en
retirant l'attente » — sauf qu'ici l'attente a été retirée en bloc, par
déménagement.

**b) Zéro entretien depuis six semaines.** Un seul commit touche les crates
neurion dans `noogram-distro` : l'import du 24 juin. Rien depuis. Pendant ce
temps, le registre a pris ~40 000 lignes de déchet.

**c) Deux copies du domaine, aucun script de synchronisation.** `cosmon/crates/neurion-core`
et `noogram-distro/plugins/mcp/crates/neurion-core` diffèrent sur 114 lignes. J'ai
vérifié le contenu de la divergence : c'est **uniquement** le scrubbing de
publication (noms de galaxies et de personnes anonymisés pour la release publique).
Donc pas de dérive logique aujourd'hui — mais rien n'empêche qu'elle arrive demain,
parce qu'aucun script ne rejoue la projection. J'ai cherché : `scripts/` des deux
côtés, rien.

**d) Un crate « frontier » Apache-2.0 qui code en dur les galaxies de l'opérateur.**
`domain/galaxy_kind.rs` contient la liste nominative des galaxies (`stagecraft`,
`musica`, `secretariat`, `chancellerie`…) dans le crate censé être `cargo add`-able
par un tiers. C'est précisément ce qui oblige à un scrubbing manuel à chaque
release — et donc ce qui crée la double copie du point (c). La donnée est dans le
code au lieu d'être dans la base que neurion *est*.

**e) Le plan annoncé n'a pas eu lieu.** `cosmon/Cargo.toml:58` dit que `neurion-mcp`
« se réhome dans la galaxie neurion ». Il n'y a pas de galaxie `neurion` dans
`~/galaxies`. Le code a atterri dans `noogram-distro/plugins/mcp`, ce qui est
défendable, mais le commentaire n'a pas suivi.

### 4.3 Le diagnostic de fond

Le développement de neurion a fonctionné comme **projet** et échoué comme
**produit**. Le sprint d'avril a bien produit un artefact cohérent. Ce qui n'a
jamais existé, c'est la boucle d'entretien : personne ne regarde la base, personne
ne mesure l'usage des outils, personne ne borne les réponses, personne ne
re-teste après un déménagement. Un système nerveux qui ne se sent pas lui-même.
Le registre pollué à 99 % en est le symptôme le plus net : **il a fallu qu'on me
demande un point d'étape pour que quelqu'un ouvre la base.**

---

## 5. État de l'art et innovation — verdict honnête

### 5.1 La couche inventaire : commoditisée

Un serveur MCP qui stocke un graphe de connaissances dans SQLite et l'expose à
l'agent est, en 2026, une catégorie **encombrée** : le serveur `memory` de
référence (entités/relations/observations), `mcp_sqlite_memory_bank`, les serveurs
knowledgegraph-mcp, `codebase-memory-mcp` (tree-sitter → KG SQLite, 14 outils),
Zep/Graphiti côté mémoire temporelle, AgentRank côté index d'outils. La couche
inventaire de neurion (mcp_servers, services, binaries, repos, configs) n'a **rien
de distinctif** face à ça. Elle n'a pas non plus de sondage actif, contrairement à
Consul, etcd ou systemd, qui font de la découverte de service *avec health-check
mesuré* depuis dix ans.

Et il faut dire le pire : pour 17 référents et 41 reaches écrits à la main, **un
fichier Markdown de 200 lignes dans le vault ferait le même travail**. Ce que la
base apporte en plus, ce n'est pas de la technique, c'est une convention : une
source unique inter-galaxies, interrogeable en SQL, écrivable à chaud
(53 `upsert_entry`), qu'on n'a pas à recopier dans 90 `CLAUDE.md`. C'est une vraie
valeur — mais c'est une victoire d'organisation, pas d'ingénierie.

### 5.2 La couche « reach » : la seule idée non banale, et elle n'est pas validée

L'abstraction intéressante est celle-ci : un **référent** (domaine logique :
« email ») est atteignable par **N porteurs** (msgvault MCP, la base SQLite brute,
des notes markdown dérivées), chacun décrit par des propriétés de canal
(couverture, interrogeabilité, fidélité, fraîcheur, latence), et le choix du
porteur est **conditionné à l'intention** (read / write / search / verify), avec un
jeu de poids différent par intention.

Ce n'est pas une idée courante dans l'outillage de mémoire d'agent. Mais il faut
être honnête sur sa nouveauté : c'est la **sélection de source à base de coût**,
c'est-à-dire un optimiseur de requêtes fédéré, ou un catalogue de virtualisation
de données (Denodo, Trino, Dremio), ou de l'OBDA — un domaine mûr depuis vingt
ans. Neurion le re-dérive à l'échelle d'un poste de travail. La transposition est
maligne ; l'invention est de la reformulation.

Et surtout : **les statistiques sont déclarées, pas mesurées.** J'ai regardé la
table `reaches` : la couverture vaut 1,0 pour presque tout, les latences sont des
nombres ronds tapés à la main, la fidélité aussi. Le score est une combinaison
linéaire de poids arbitraires (35 % / 25 % / 15 % / 15 % / 10 %) appliquée à des
nombres arbitraires. **Rien n'a jamais vérifié qu'un score plus haut donne une
meilleure réponse.** Un optimiseur de requêtes sans statistiques collectées, c'est
un optimiseur qui devine — la différence entre un thermomètre et une étiquette où
quelqu'un a écrit « il fait bon ».

La typologie de santé (`GAP` / `FRAGILE` / `SHALLOW` / `SLOW` / `STALE` / `HEALTHY`)
est le meilleur morceau de tout le projet : elle est simple, elle nomme un trou
dans la topologie d'accès, elle coûte trois lignes de SQL. Mais sur 17 référents
saisis à la main, elle rapporte ce que l'opérateur savait déjà.

### 5.3 Verdict

**Utile mais banal dans sa catégorie, avec une idée mildly originale (le reach à
intention) jamais calibrée, et une moitié haute franchement sur-conçue** —
hypergraphe n-aire à rôles et ordinaux, paramètres de Shannon par surface,
détection de dérive de famille de galaxies : de l'infrastructure conceptuelle pour
un volume de données qui tient dans un tableur, et zéro appel en cinq mille
sessions.

Ce n'est pas un échec. C'est un **prototype qu'on a pris pour un produit** parce
qu'il était bien écrit et bien documenté. La qualité de la prose des ADRs et la
qualité de l'ingénierie ont été confondues.

---

## 6. Neurion comme socle de notarisation des sessions de pilotage

Rappel de la demande (`idea-20260803-313f`, axe 4) : *« retrouver ce qui s'est
passé sans dupliquer les logs provider (les référencer) — candidat DB locale type
Neurion »*, sous la thèse centrale *« aucune instruction ne doit être perdue »*.

### 6.1 Ce que neurion apporte vraiment : le motif, et il est bon

Le motif **pointeur-seul** est exactement ce qu'il faut, et neurion l'a déjà
implémenté deux fois avec discipline :

- `chronicles` indexe `(id, galaxie d'origine, chemin relatif, date, titre, phrase
  de principe, citations)` — **le contenu reste dans la galaxie**, neurion ne
  stocke que le pointeur.
- `PersonSurface` (ADR-005) va plus loin : liste blanche de colonnes au niveau du
  schéma + test anti-fuite à sentinelle, pour garantir que le corps de la fiche ne
  peut pas entrer dans la base.

La formule du doc est juste et transposable telle quelle aux sessions : *« mets à
jour la fiche et la réponse se met à jour toute seule, parce que le registre
stocke un pointeur qui ne peut pas pourrir. »* Pour des logs provider qu'on veut
référencer sans copier, c'est le bon principe.

### 6.2 Pourquoi l'instance neurion ne peut pas être ce socle

| Exigence d'un socle de notarisation | État de neurion |
|---|---|
| Intégrité / inaltérabilité | **aucune**. `upsert_entry` et `delete_entry` sont exposés en écriture libre à tout agent. Pas de hash-chaîne, pas de signature, pas de journal append-only. |
| Horloge / historique | **aucun**. Une seule colonne `updated_at` écrasée à chaque écriture. On ne peut pas répondre « qu'est-ce que la base disait le 12 juillet ? ». |
| Modèle de session | **inexistant**. Pas de notion de session, d'instruction, de chaîne de raisonnement. Tout serait à construire. |
| Isolation | **aucune**. Namespace global unique, mélangé avec l'inventaire d'environnement. |
| Propreté du substrat | **compromise**. La base est à 99,2 % de déchet de test et grossit de 1 000 lignes/jour. On ne pose pas un registre notarial sur un plancher qui prend l'eau. |
| Discipline de réponse | **absente**. Une requête peut rendre 3 Mo. Un archivage se consulte ; s'il noie le consultant, il n'archive rien. |
| Entretien | **nul depuis le 24 juin**. |

Il y a un point plus dur que tous les autres. Un registre de notarisation doit être
**append-only et vérifiable**. Neurion est, par construction et par API, une base
mutable que n'importe quel agent peut réécrire ou effacer. Ce n'est pas un défaut
qu'on corrige avec un patch : c'est l'inverse de la propriété demandée.

### 6.3 Et le fait que cosmon a déjà l'outil

C'est l'argument décisif. Le noyau contient déjà :

- **`cosmon-notary`** (ADR-056) — engagements signés Ed25519 sur des hash de
  contenu, « Proof-of-Sealed-Presence », avec un emplacement réservé pour le reçu
  d'énergie fourni par `claudion`. Nommage honnête, crate isolé pour la
  surface d'approvisionnement, trait `signature::Scheme` pour changer d'algorithme.
- **`cosmon-pilot-checkpoint`** — persistance d'état de pilotage.
- **`cosmon-process-witness`**, **`cosmon-hash`**, **`cs sessions`** (M0–M6 livrés),
  `energy_probe`.

Poser la notarisation dans neurion reviendrait à écrire un deuxième mécanisme
d'attestation, plus faible, à côté de celui qui a été conçu pour ça.

### 6.4 Recommandation

**Non comme instance. Oui comme motif — et le motif, on l'a déjà écrit.**

Concrètement, pour l'axe 4 du cockpit :

1. **Store dédié**, propre au cockpit, hors de `neurion.db` : un SQLite
   `sessions.db` **append-only**, une ligne = un événement (instruction opérateur,
   dispatch, molécule touchée, verdict), jamais de `UPDATE`, jamais de `DELETE`.
2. **Pointeur, pas copie** : chaque événement référence le log provider par
   `(chemin, offset, hash du blob)`. Reprendre littéralement la discipline
   `chronicles` + la liste blanche de colonnes de `PersonSurface`, y compris le
   test à sentinelle qui prouve que le corps ne peut pas entrer. C'est le seul
   morceau de neurion à transplanter, et il vaut le voyage.
3. **Scellement par `cosmon-notary`** : chaîner les hash d'événements, sceller
   périodiquement la tête de chaîne en PoSP. C'est ce qui transforme « une base de
   logs » en « on peut prouver que rien n'a été retiré ».
4. **Ce qu'on garde de neurion dans le cockpit** : rien d'autre. Éventuellement un
   `reach` vers le store de sessions, pour que l'agent sache qu'il existe — c'est
   le rôle légitime de neurion, être l'annuaire, pas le coffre.

Et, indépendamment du cockpit, **trois réparations à faire sur neurion lui-même**,
par ordre de rendement (aucune n'est faite ici, la molécule est une analyse) :

- **P1** — étendre `is_excluded_repo_path` aux répertoires temporaires
  (`std::env::temp_dir()`, `/private/var/folders`, `/tmp`) **et** purger les
  21 322 lignes existantes. Une ligne de prédicat, un `DELETE`. C'est le meilleur
  rapport effort/effet du lot.
- **P2** — borner toutes les réponses d'outil (`LIMIT` + pagination), en
  commençant par `list_services` et la queue `LIKE` de `how_to_access`. Un outil
  MCP qui peut rendre 3 Mo est un piège pour la session qui l'appelle.
- **P3** — décider franchement pour les étages morts : soit on branche
  l'hypergraphe et la détection de dérive à quelque chose de réel, soit on les
  retire. Ordre de grandeur : ~550 lignes pour `drift` seul, plus la couche
  hypergraphe répartie entre `schema.rs`, `sqlite.rs` et `tools.rs`. Du code non
  appelé dans un crate publié, c'est de la dette qu'on paie à chaque relecture et
  à chaque release.

---

## 7. Falsifieurs

Ce verdict est faux si l'une de ces affirmations est démontrée :

1. **« Le registre n'est pas pollué. »** Réfuté si
   `SELECT COUNT(*) FROM repos WHERE local_path LIKE '/Users/%'` remonte à plus de
   50 % du total. Mesuré ce jour : 164 / 21 486 = 0,76 %.
2. **« Les étages hauts servent. »** Réfuté si on trouve, dans les journaux de
   session, ne serait-ce qu'**un** appel réel à `add_node`, `add_edge`,
   `query_graph` ou `describe_node`, ou **un** appelant de `hub_to_project_drift`
   et consorts hors tests. Mesuré : zéro des deux côtés.
3. **« Le classement des reaches est validé. »** Réfuté si quelqu'un exhibe une
   mesure — même grossière — comparant le porteur choisi par `intent_score` au
   meilleur porteur constaté a posteriori. Je n'ai trouvé aucun harnais de ce type.
   Ce falsifieur est le plus important : s'il tombe, le verdict « statistiques
   déclarées, jamais calibrées » tombe avec, et la section 5.2 doit être réécrite.
4. **« La suite de tests n'a pas été amputée. »** Réfuté si les 111 tests de
   `tests.rs` d'avril ont un équivalent ailleurs dans le plugin. Vérifié par
   comptage sur les deux arbres : 111 → 13, et `domain/resolver.rs` (10 tests) a
   disparu sans destination.
5. **« Neurion peut être rendu notarial par un patch. »** Réfuté si on montre un
   chemin qui rend le store append-only **sans** retirer `upsert_entry` et
   `delete_entry` de la surface MCP — c'est-à-dire sans casser les 53 appels
   d'écriture qui constituent aujourd'hui l'usage vivant du serveur.
6. **« L'usage décline. »** Le zéro appel en août porte sur 4 jours seulement :
   trop court pour conclure. Réfuté si août termine au-dessus de ~50 appels. À
   re-mesurer fin août avant d'en tirer quoi que ce soit.

---

## 8. Ce que je ne dis pas

- Je ne dis pas que neurion est à jeter. Il est **load-bearing** pour cosmon
  aujourd'hui : `GalaxyKind` alimente `cs galaxies`, `cs mur`, `cs status` ;
  `schema::SCHEMA_SQL` est exécuté par `cs init` et `cs doctor`. On ne le retire
  pas sans travail.
- Je ne dis pas que l'architecture est mauvaise. L'hexagone tient sur le point où
  il casse d'habitude, et je l'ai vérifié plutôt que supposé.
- Je ne dis pas que le motif pointeur-seul est banal. Il est bon, et il est la
  seule chose de neurion que je recommande de transplanter dans le cockpit.

Ce que je dis, c'est que la distance entre la qualité de la prose et la qualité de
l'entretien est devenue très grande, et que le registre à 99 % de déchet en est la
mesure la plus honnête.

---

*Sources externes consultées pour le point 5 (paysage 2026)* :
[mcp_sqlite_memory_bank](https://github.com/robertmeisner/mcp_sqlite_memory_bank) ·
[Knowledge Graph MCP servers](https://mcpservers.org/servers/n-r-w/knowledgegraph-mcp) ·
[codebase-memory-mcp](https://deusdata.github.io/codebase-memory-mcp/) ·
[Codebase-Memory (arXiv 2603.27277)](https://arxiv.org/html/2603.27277v1) ·
[Infrastructure for the Agentic Web (arXiv 2606.20570)](https://arxiv.org/pdf/2606.20570) ·
[Data virtualization (Databricks)](https://www.databricks.com/blog/what-is-data-virtualization)
