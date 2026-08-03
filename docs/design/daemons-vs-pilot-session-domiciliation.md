# Délibération — les procédures d'une session pilote doivent-elles descendre dans `daemons.toml` / `patrols.toml` ?

**Molécule :** `task-20260803-c7ef` — délibération, aucune implémentation, aucun
changement de configuration.
**Date :** 2026-08-03. **Siège :** Claude.
**Entrées lues :** le CMB `musica-pornic/docs/cmb/outbox/2026-08-03-to-cosmon-daemons-sessions-pilot.md`
(diagnostic mesuré, Q1–Q7) ; [ADR-168](../adr/168-a-co-pilot-inherits-the-session-substrate-not-its-delivery-contract.md)
et ses trois pièces (trace-a-claude, trace-b-codex, probe-log) ; le brief de
mission co-pilotage multi-provider (stagecraft CMB, 2026-07-24) ;
`~/.config/cosmon/daemons.toml` (21 daemons) et `patrols.toml` (23 patrouilles) ;
`cs sessions --help` (M5+M6 sur main), `cs run --resident`, `cs presence`,
`cs whisper`, `cs init` ; ADR-053 (superviseur de daemons), ADR-050 (scheduler) ;
`docs/guides/pilot-portability.md` et le mécanisme `install.sh --pilot-pack`.

---

## 0. La réponse en une phrase

Oui pour tout ce qui **ne détruit rien**, non pour le reste — et la frontière
n'est pas « automatisable / non automatisable » mais **« annulable en effaçant un
fichier / pas annulable »**. Un daemon peut *réveiller* un pilote ; il ne peut pas
*être* le pilote.

---

## 1. Le critère (Q1)

La question demande un critère, pas une liste. Le voici, en une ligne :

> **Le test de la corbeille.** Une procédure a le droit d'être déclarative — une
> entrée dans `daemons.toml` ou `patrols.toml` — si et seulement si **tout effet
> qu'elle peut produire s'annule en effaçant un fichier qu'elle a écrit**. Sinon
> elle appartient à un esprit : session pilote ou opérateur.

C'est un test, pas un principe : on l'applique en se demandant « si ça part de
travers pendant la nuit, est-ce que `rm` suffit à revenir en arrière ? ».

### 1.1 Ce que le test place, et où

| Geste | Effet | `rm` suffit ? | Domicile |
|---|---|---|---|
| Poller Telegram → inbox | écrit un `.txt` | oui | daemon ✅ *(déjà)* |
| `cosmon-tg-route`, `cosmon-issue-watch` | écrit / route un fichier | oui | daemon ✅ *(déjà)* |
| `cs presence ping` (heartbeat) | écrit un snapshot | oui | daemon/hook ✅ *(M6)* |
| Relève de mailbox + ack | ajoute une ligne d'ack, livraison au-moins-une-fois | oui | daemon/hook ✅ *(M6)* |
| Mesure de coût (`claudion`, `codex_energy`) | append-only | oui | patrouille ✅ |
| *Staging* d'un checkpoint | écrit un brouillon, écrasé/supprimé | oui | hook ✅ *(M6)* |
| Notifier l'opérateur | envoie un message | non, mais sans effet sur l'état | daemon ✅ |
| `cs tackle` | dépense du crédit, crée un worktree | **non** — effacer ne rend pas les jetons | session |
| `cs done` / merge | réécrit `main` | **non** | session/opérateur |
| `cs collapse`, bras collapse de `cs purge` | détruit un travail en cours | **non** | opérateur |
| `takeover grant` (bail PRIMARY) | déplace l'autorité | **non** | opérateur seul (ADR-168 D6) |

### 1.2 Pourquoi ce critère et pas un autre : il rétrodit les deux accidents

Un critère qui n'explique que ce pour quoi on l'a écrit ne vaut rien. Celui-ci
prédit, sans avoir été construit pour, les deux dégâts déjà mesurés sur cette
machine :

1. **`cosmon-runtime` (`cs run --resident`) est désactivé depuis le 2026-07-23**
   parce qu'il « martelait en boucle des molécules #21 exigeant claude et a
   corrompu des métadonnées » (commentaire dans `daemons.toml`). Le test de la
   corbeille le refuse d'emblée : le résident appelle `tackle` et `done`, deux
   gestes qu'aucun `rm` ne défait. Il n'a pas été désactivé par malchance ; il
   était **mal domicilié**.
2. **`cs purge` a auto-collapsé 4 molécules après un reboot**, travail non
   moissonné, récupéré à la main. Même refus : le collapse détruit. Le bras
   « nettoyer des fichiers morts » de `purge` passe le test ; le bras
   « collapse » ne le passe pas et n'aurait jamais dû tourner sans main humaine.

À l'inverse il ratifie ce qui a survécu : les bots Telegram, `cosmon-tg-route`,
`cosmon-issue-watch` ont traversé les reboots et les cycles de session, et ce
sont exactement les procédures que le test autorise.

### 1.3 Ce que devient le résident (branche (b) de Q1)

Sous ce critère, `cs run --resident` dans sa forme actuelle **n'a pas de
domicile** : c'est un daemon qui mute. Sa moitié légitime est le calcul de
lisibilité du DAG — quelles molécules sont prêtes, laquelle d'abord. Cette
moitié écrit un fichier et rien d'autre.

Recommandation : le résident devient un **proposeur**. Il publie une file
`ready` inspectable (`.cosmon/state/ready-queue.jsonl`) et n'appelle plus jamais
`tackle`/`done`. Le geste de dépense revient à un esprit. C'est un rétrécissement
de surface, pas un ajout : le résident perd deux appels et gagne un fichier.

---

## 2. Le vrai problème derrière la question : la flotte qui retombe à zéro

Le test de la corbeille interdit à un daemon de nourrir la flotte. Or la mesure
du 2026-08-01→03 dit que la flotte retombe à zéro entre deux vagues, et que le
coût du pilotage manuel est réel. Si on s'arrête au critère, on a raison et la
flotte dort.

La sortie n'est pas d'assouplir le critère. C'est de remarquer **où on l'a
appliqué trop tôt** : le geste interdit au daemon, c'est de *muter*. Rien ne lui
interdit de **réveiller un esprit qui, lui, a le droit**.

C'est exactement ce que `musica-tg-route` fait déjà et qui marche : il ne répond
pas lui-même, il décroche et réveille une session headless avec un mandat étroit.
Ce qui manque du côté cosmon, ce n'est pas le mécanisme — c'est **l'autorité
bornée** de la session réveillée. Et cette autorité existe déjà, écrite et
livrée : `PilotLease { mission_id, holder_session_id, epoch, granted_by,
granted_at, **expires_at** }` (ADR-168 D5, M4 `task-20260731-9cf4`), avec le garde
qui refuse une époque périmée *avant* effet.

D'où la recommandation unique.

---

## 3. Recommandation UNIQUE — « le relais de veille »

Une seule phrase à retenir : **le daemon ne mute jamais ; il réveille une session
qui détient un bail borné dans le temps, accordé par l'opérateur.**

Quatre conséquences, dans cet ordre :

**R1 — Adopter le test de la corbeille comme critère de domiciliation**, et le
faire passer sur l'existant. Ce qui le passe descend dans `daemons.toml` /
`patrols.toml`. Ce qui ne le passe pas remonte à une session, sans exception
tolérée « parce que ça a toujours marché ».

**R2 — Migrer les procédures conservatrices du pilote vers le déclaratif.**
Capture, présence, relève de courrier, mesure de coût, staging de checkpoint,
notification. Aucune n'est un mécanisme nouveau : M6 (`task-20260731-0d49`) a déjà
livré la moitié exécutable (`cs sessions hook install --provider {claude,codex}`).
Ce qui manque n'est pas du code, c'est le fait que le hook **n'est installé nulle
part** : le CMB constate qu'aucun hook `cs sessions` n'était posé, ni pour Claude
ni pour Codex, dans la galaxie cosmon observée. C'est la cause directe de
« présence sans lecteur » (fragilité 3) et de l'illusion de continuité
(fragilité 1).

**R3 — Le réveil, pas l'injection** (réponse frontale à Q2 du CMB). Deux contrats
nommés séparément, jamais l'un en repli silencieux de l'autre :
- **réveiller** = un daemon lance un processus neuf avec un *pack de bootstrap*
  explicite et un bail à `expires_at` court. Passe le test de la corbeille : le
  daemon n'écrit qu'un fichier de demande ; c'est la session qui mute, sous bail.
- **injecter** = déposer une enveloppe dans la mailbox d'une session vivante, qui
  la consommera à sa prochaine frontière de tour. Ne mute rien. Déjà livré (M2/M6).

Un daemon qui ne trouve pas de session fraîche **ne doit pas** basculer en
mutation directe. Il réveille, ou il laisse le message durable et non-livré. Le
repli silencieux est précisément ce qui a fabriqué l'illusion mesurée à Musica.

**R4 — Le transport s'installe, il ne se raconte pas** (réponse à Q2 de la
mission, en tenant compte de Q3/Q7 du CMB). Voir §4.

---

## 4. Où vivent les instructions de TRANSPORT (Q2)

**Oui, dans la config globale du harnais — mais jamais écrites à la main, et
jamais la source de vérité.**

Le précédent existe déjà dans ce dépôt et fonctionne : `install.sh --pilot-pack`
(assets de `cosmon-rpp-adapter`) pose un bloc délimité

```text
# >>> cosmon pilot-pack >>>
…
# <<< cosmon pilot-pack <<<
```

dans le fichier d'instructions du harnais, avec sa source de vérité dans
`~/.config/cosmon/pilot.AGENTS.md` et un ordre de rafraîchissement imprimé dans
le bloc lui-même. C'est déjà la bonne forme pour le pilote *distant*. Il manque
l'équivalent pour le pilote *local*.

Trois propriétés à exiger, chacune répondant à une fragilité mesurée :

1. **Bloc délimité et idempotent** — le harnais peut avoir ses propres
   instructions autour ; l'installation ne les touche pas et se rejoue sans
   dupliquer.
2. **Source de vérité côté cosmon, pas côté harnais** — sinon on obtient un
   texte que personne ne régénère et qui dérive. Le bloc dit d'où il vient.
3. **Octets identiques pour tous les providers** — c'est le test de
   provider-neutralité (Q7), et il est vérifiable mécaniquement : le corps du
   pack pour Claude et pour Codex doit être *le même fichier*. Seule la
   **destination** diffère (`CLAUDE.md` vs `AGENTS.md` / config codex), fournie
   par l'adaptateur. Si le corps diverge d'un octet, le contrat n'est pas
   provider-neutre et c'est le port qui est à corriger — même forme que le
   falsifier 10 d'ADR-168.

**Quel verbe l'installe ?** Pas `cs init` : `cs init` amorce un *projet*
(galaxie), alors que le contrat de transport est *par hôte × par harnais*.
Poser un pack de transport dans chaque `.cosmon/` créerait N copies dérivantes du
même paragraphe. Pas non plus un `cs pilot install` neuf : `cs pilot` est le REPL
cognitif d'ADR-115 (D3.5 refuse déjà de l'absorber), et la surface CLI n'a pas
besoin d'un verbe de plus.

**Recommandation : étendre le verbe qui existe déjà** — `cs sessions hook
install --provider <p>` pose *les deux moitiés* : le hook qui agit (présence,
mailbox, checkpoint staged) **et** le paragraphe qui explique au pilote ce que le
hook fait et comment l'adresser. Le pack atterrit au même niveau que le hook
(hook projet → fichier d'instructions du projet ; hook global → fichier global).

Le gain n'est pas l'économie d'un verbe, c'est **l'impossibilité de la dérive** :
le mécanisme et son mode d'emploi sont posés par le même geste, donc ils ne
peuvent pas se désynchroniser. Aujourd'hui ils le peuvent, et le CMB a mesuré le
résultat — un routeur qui écrit dans un canal que personne ne relève.

**Ce que le pack doit contenir, et rien de plus** (bootstrap provider-neutre,
Q3 du CMB) :

```text
identité canonique      <provider>:<native-session-id> — un titre est un alias, jamais une adresse
canal entrant           cs sessions inbox — à relever, personne ne pousse dans ton terminal
canal sortant           cs sessions send --to <selector>
autorité                ton bail et son époque ; sans bail, lecture seule
mission courante        molécule racine + checkpoint courant
classes de message      courrier tiers | conseil de co-pilote | geste opérateur (§Q6)
diagnostic              cs sessions hook status ; COSMON_COPILOT_HOOK_OFF=1
```

Le `cwd`, `CLAUDE.md` et le filesystem restent utiles et **ne sont pas le
protocole**. C'est le point 4 des fragilités du CMB, et le pack est sa réponse :
ce qui doit être vrai pour qu'une session soit adressable devient explicite et
borné, au lieu d'être reconstruit par inférence.

---

## 5. Rapport à la mission co-pilotage (Q3)

**Verdict : mission distincte, en aval, non bloquante pour M7/M8.**

C'est le même problème vu d'un autre angle — « qui a le droit de muter, et
comment on le lui remet » — mais l'angle change le destinataire. Le co-pilotage
répond pour **deux esprits**. Ici la question est : **un daemon est-il un
correspondant légitime sur ce rail ?**

La réponse tombe du modèle d'autorité déjà accepté :

> **Un daemon est un co-pilote sans bail.**

Il observe, il capte, il écrit, il *demande* — exactement les droits que
D6 accorde à `role: COPILOT`. Il ne mute pas, parce qu'il ne peut pas tenir un
bail : un bail est accordé à un esprit responsable, et une entrée TOML n'en est
pas un. Cette phrase règle Q1 et Q6 du CMB d'un coup — le courrier d'un tiers, le
conseil d'un co-pilote et le geste d'un pilote sont trois classes qui se
distinguent par le bail présenté dans l'enveloppe, pas par le canal emprunté.

**Frontière exacte avec M5 (`cs sessions`).** M5 donne aux *sessions* une adresse
et un cockpit humain. La mission proposée ici ne touche pas à cette surface : elle
ajoute (a) un **écrivain non-session** légitime sur le même rail, (b) le **réveil**
comme contrat nommé, (c) l'**installation** du pack. Le critère de non-empiètement
est celui du falsifier 10 : *si cette mission oblige à modifier `cs sessions`,
c'est qu'elle a dérapé dans M5*. Un seul ajout est admis dans `cs sessions`, et
c'est celui de R4 — un effet de bord d'installation sur le verbe `hook install`,
sans nouveau sous-verbe.

**Ordre.** M7 (dogfood shadow) et M8 (exercice de relève) n'ont besoin de rien de
ceci. Mieux : **M7 doit passer d'abord**, parce que son critère d'acceptation
inclut « friction opérateur listée ». C'est cette liste, mesurée, qui dira quels
gestes manuels reviennent assez souvent pour mériter une entrée TOML. Nucléer
cette mission avant M7, c'est deviner la liste au lieu de la mesurer.

---

## 6. Ce qui ne doit PAS être automatisé (Q4)

Le critère donne la réponse ; voici les gestes nommés et le *pourquoi* de chacun,
parce que « le critère l'interdit » n'est pas une raison qu'on peut vérifier.

| Geste | Reste chez | Raison irréductible |
|---|---|---|
| `cs done` / merge sur `main` | opérateur / session sous bail | Ce n'est pas la mécanique du merge qui est humaine, c'est le **jugement « ce travail est bon »**. Aucun fichier effacé ne défait un `main` réécrit. Déjà human-only (ADR-079 §4). |
| `cs tackle` | session sous bail | Dépense un crédit irréversible et choisit *quoi* faire maintenant. Un daemon qui choisit la priorité pilote la mission sans en porter le contexte — le F5 mesuré du 23 juillet. |
| `cs collapse` et le bras collapse de `cs purge` | opérateur | Détruit du travail non moissonné. Mesuré le 2026-08-03 : 4 molécules, récupérées à la main. Le nettoyage de fichiers morts, lui, passe le test et peut rester automatique. |
| `takeover grant` (accorder/transférer le bail) | opérateur seul | ADR-168 D6 et D3.1 : ni un pilote, ni une lecture de quota, ni un trou de heartbeat n'exécute un transfert. Un daemon qui accorde un bail s'accorde l'autorité à lui-même. |
| Le **contenu** d'un checkpoint | pilote | ADR-168 : le *moment* appartient au hook, le *contenu* au pilote. Un hook qui remplit le contenu publie les positions d'un esprit qui ne les a jamais tenues — et `cs sessions drift` les comparerait comme si. |
| Répondre à un tiers en son nom | opérateur / session sous mandat étroit | Le courrier d'un tiers est une donnée à interpréter, jamais une autorité (contrainte du CMB). |
| Décider qu'une session « est » le pilote | opérateur | Le titre `pilot` est un alias UX. Une résolution automatique d'alias ambigu est le falsifier 4 d'ADR-168. |

Un cas mérite d'être nommé à part, parce qu'il est tentant et qu'il est refusé
pour une raison **empirique** et non doctrinale : **le déclenchement sur quota**.
Trace A d'ADR-168 le montre — une session Claude ne publie pas de signal de quota
*approchant*, seulement l'erreur une fois arrivée. Un daemon qui « voit venir »
la limite lirait en réalité la jauge de l'*autre* pilote. Il n'y a pas
d'automatisation à construire là ; il n'y a pas de signal.

---

## 7. Falsifiers

La recommandation est fausse si l'une de ces observations tient. Chacune est
vérifiable sur cette machine.

1. **Une procédure qui passe le test de la corbeille cause une perte de travail
   une fois posée en daemon.** Le critère serait trop laxiste, et il faudrait une
   seconde clause (probablement sur la dépense externe).
2. **Un geste qui échoue au test doit malgré tout être automatisé pour que la
   flotte tienne**, et le réveil-sous-bail ne suffit pas à le couvrir. Alors R3
   est un pansement et le vrai besoin est un régime d'autonomie assumé.
3. **Le pack de transport diverge d'un octet entre Claude et Codex** pour une
   raison qui n'est pas la destination du fichier. Alors le contrat n'est pas
   provider-neutre et c'est le port M1 qu'il faut corriger, pas le pack (analogue
   du falsifier 10).
4. **Une session réveillée avec le pack de §4 ne peut toujours pas répondre
   pertinemment** — il lui manque du contexte que le pack ne nomme pas. Alors le
   bootstrap est sous-spécifié et l'expérience 8 du CMB (« lancer un headless neuf
   depuis le même checkpoint et mesurer ce qui lui manque ») est le protocole qui
   le dira.
5. **Un bail à `expires_at` court produit des mutations à moitié faites** au
   moment de l'expiration (worker orphelin, molécule `running` sans pilote).
   Alors le bail borné n'est pas le bon instrument de bornage et il faut borner
   par *nombre de gestes* plutôt que par temps.
6. **Cette mission oblige à modifier `cs sessions`** au-delà de l'effet de bord
   d'installation de R4. Alors elle a dérapé dans M5 et la frontière de §5 est
   mal tracée.
7. **Deux daemons se retrouvent à écrire dans la même mailbox de session** avec
   des séquences qui se marchent dessus. Alors « un daemon = un co-pilote sans
   bail » est insuffisant : il faut aussi une identité de daemon dans l'enveloppe,
   là où le CMB note déjà que l'identité d'expéditeur est le nom d'utilisateur OS
   et ne distingue pas deux écrivains sur un même hôte.

---

## 8. Découpage en molécules proposées (NON nucléées)

Sept molécules. L'ordre est contraint par les *write-sets* autant que par la
logique : deux molécules qui touchent les mêmes fichiers sont sérialisées, même
si elles sont logiquement indépendantes.

**Porte d'entrée : M7 du co-pilotage doit être terminale et sa liste de friction
opérateur écrite.** Avant, on devine ; après, on mesure.

| # | Molécule | Livrable | Write-set principal | Dépend de |
|---|---|---|---|---|
| D1 | **ADR — le test de la corbeille** | ADR qui grave le critère de §1, ses deux rétrodictions et ses falsifiers. Doc-only, aucun octet de CLI. | `docs/adr/` | porte M7 |
| D2 | **Audit de domiciliation de l'existant** | Passage du test sur les 21 daemons + 23 patrouilles + le résident. Verdict par entrée : *reste*, *descend*, *remonte*. Rapport seul, aucune config modifiée. | `docs/audit/` | D1 |
| D3 | **Re-scoper `cs run --resident` en proposeur** | Le résident publie `ready-queue.jsonl` et n'appelle plus `tackle`/`done`. Test qui échoue d'abord : « le résident ne peut pas dépenser ». | `crates/cosmon-cli/src/cmd/run.rs` | D1 |
| D4 | **Désarmer le bras collapse de `cs purge`** | `purge` nettoie les fichiers morts ; le collapse d'une molécule non moissonnée exige un geste explicite. Test de régression sur les 4 molécules du 2026-08-03. | `crates/…/cmd/purge.rs` | D1 |
| D5 | **Pack de transport + installation** | Source de vérité côté cosmon ; `cs sessions hook install` pose le bloc délimité idempotent ; test d'égalité octet-à-octet du corps entre providers. **Dette CLI : `cs help` + `man cs` + parity audit dans le même PR.** | `crates/cosmon-cli/src/cmd/sessions/hook.rs`, assets, `man/`, snapshots | D1 |
| D6 | **Contrat de réveil nommé** | `réveiller` et `injecter` comme deux chemins distincts ; interdiction du repli silencieux de l'un vers l'autre ; le réveil produit une demande de bail, jamais une mutation. Test : aucune session fraîche ⇒ message durable non-livré, **jamais** de mutation directe. | `crates/cosmon-core/src/pilot_lease.rs` (+ appelants) | D5 |
| D7 | **Dogfood : une nuit sous relais de veille** | Une vague de flotte tenue une nuit par réveil-sous-bail. Mesure : molécules avancées, gestes refusés par le garde, travail perdu (cible : zéro), friction opérateur restante. | `docs/measurements/` | D3, D4, D6 |

Sérialisation imposée par les write-sets : **D5 avant D6** (les deux touchent la
surface `cs sessions` et ses snapshots ; en parallèle elles se marchent dessus).
D3 et D4 sont indépendantes l'une de l'autre et de D5 — trois fichiers disjoints,
elles peuvent partir ensemble après D1.

D7 est la seule qui produit une mesure, et c'est elle qui décide si la
recommandation tient. Les six autres ne sont que ce qu'il faut construire pour
pouvoir la faire.

---

## 9. Ce que cette délibération ne tranche pas

- **Le rail canonique de messagerie** (Q5 du CMB) — deux rails coexistent,
  `presence/<sid>.log` + `poll` et la mailbox `<sid>.inbox.jsonl` de M2. ADR-168
  explique pourquoi la mailbox n'a pas de curseur d'octets et pourquoi le legacy
  garde le sien, mais **ne fixe pas la doctrine de migration**. C'est une décision
  à part entière, qui appartient à la mission co-pilotage, pas à celle-ci.
- **Les permissions effectives de Codex** sur `.git`, l'état cosmon et les
  worktrees (Q7 du CMB). Une session qui lit mais n'intègre pas n'est pas un
  PRIMARY opérationnel — c'est M7/M8 qui le mesureront, sur pièces.
- **Le régime d'autonomie visé.** Le relais de veille tient la flotte éveillée
  sous bail borné. Il ne répond pas à la question « cosmon doit-il pouvoir
  avancer sans aucun esprit pendant N jours ». Cette question-là est ouverte et
  n'est pas déclenchée par la présente mesure.

---

*Livrable de `task-20260803-c7ef`. Aucune configuration modifiée, aucune molécule
nucléée, aucun daemon ajouté ou retiré par cette molécule.*
