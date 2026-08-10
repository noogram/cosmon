<!-- SPDX-License-Identifier: AGPL-3.0-only -->

# Référent notarisable d’un segment de log provider — spécification v1 expérimentale

Statut : **verdict expérimental, sans intégration notary ni choix de magasin**.
Cette spécification sépare deux propriétés souvent confondues : l’identité
stable des octets observés et leur disponibilité future.

## Verdict

Le bon identifiant est le hash BLAKE3 du **segment exact d’octets complets
consommé**, avec séparation de domaine et longueur encadrée. Il survit à un
changement de chemin, à la reprise du lecteur et à la disparition du fichier,
et transforme rotation, troncature ou réécriture en résultats vérifiables
(`Verified`, `Mismatch`, `Missing`). Il ne permet pas de reconstruire des
octets effacés.

Le falsifier demandé est donc atteint pour la propriété opérateur forte
« retrouver ce qui s’est passé après effacement » : **aucun référent sans copie
indépendante ne peut rendre disponible un contenu que le provider a détruit**.
Un hash qui subsiste n’est pas un log qui subsiste. Si la notarisation doit
garantir la relecture après effacement provider, la contrainte « ne pas
dupliquer » doit être ré-ouverte. Si elle exige seulement preuve d’identité et
détection honnête de l’indisponibilité, ce référent suffit.

## Forme canonique

Pour un segment contigu `bytes` arrêté après la dernière ligne complète :

```text
segment_id = BLAKE3(
  "cosmon-session-probe/v1/segment\0" ||
  little_endian_u64(len(bytes)) ||
  bytes
)

text = "blake3:" || lower_hex(segment_id)
reference = { id: text, byte_length: len(bytes) }
```

Les octets sont ceux du provider, y compris séparateurs de lignes et encodage.
Il est interdit de hasher les événements normalisés, le JSON ré-émis ou le
texte décodé avec remplacement : ces transformations ne sont pas le contenu
consommé et peuvent faire converger deux sources différentes.

Provider, `native_session_id`, chemin, offsets, timestamps et cause de
continuité sont des **indices de routage/provenance**, pas l’identité. Ils
peuvent accompagner une notarisation, mais ne participent pas au hash : une
rotation ne doit pas renommer le contenu.

## Résolution derrière un port

Le cœur expose `SegmentResolver`, sans filesystem, Neurion ni service distant
choisi. Un adaptateur cherche des octets candidats par ses propres indices,
recalcule la référence, puis retourne exactement un des états suivants :

| État | Signification |
|---|---|
| `Verified` | des octets disponibles reproduisent `id` et `byte_length` |
| `Mismatch { observed }` | un candidat existe, mais il a été compacté, tronqué ou remplacé |
| `Missing` | aucun candidat n’est disponible ; ce n’est pas une preuve que le contenu n’a jamais existé |
| `Err` | le mécanisme de résolution a échoué ; à ne pas confondre avec `Missing` |

Une notarisation future doit signer la référence et ses métadonnées de
provenance via le commitment Ed25519 de `cosmon-notary`. Le champ
`energy_receipt` reste réservé au PoLE : cette spécification ne le détourne
pas. L’implémentation de capture devra produire la référence sur les octets
bruts au même seam que le curseur, avant le décodage avec pertes. Ce lot ne
réalise volontairement ni cette intégration, ni l’écriture archive/notary, ni
un backend de `SegmentResolver`.

Pour une session en plusieurs segments, la notarisation conserve la liste
ordonnée des références et peut inclure `previous_segment_id` comme provenance.
Ce lien d’ordre n’entre pas dans `SegmentId` : deux consommateurs des mêmes
octets doivent obtenir la même identité, même s’ils les rencontrent dans des
chaînes différentes.

## Alternatives falsifiées

| Proposition | Résultat |
|---|---|
| `timestamp + provider_session_id` | désigne une session mutable, pas les octets ; ne détecte ni compaction ni réécriture et dépend de l’autorité provider |
| chemin + plage d’octets | utile comme indice de routage seulement ; le même chemin et les mêmes offsets changent de sens après rotation/troncature |
| chaîne incrémentale façon `cosmon-archive` | prouve ordre et préfixes uniquement si chaque maillon demeure disponible ; sans copie des segments, un maillon supprimé reste irrésolvable |
| arbre de Merkle par segment | apporte des preuves d’appartenance et mises à jour logarithmiques dont une séquence append-only n’a pas besoin ; ne crée aucune disponibilité |
| copie dans `cosmon-archive` | satisfait disponibilité et intégrité, mais duplique précisément les logs ; c’est une autre décision opérateur, pas un référent « sans copie » |

Le hash de segment gagne donc sur identité, déduplication et détection. Il ne
« gagne » pas sur l’effacement : aucune des alternatives sans réplication ne
le peut.

## Preuve expérimentale

La preuve exécutable est
`crates/cosmon-session-probe/tests/notarisable_reference.rs`. Elle s’exécute
avec :

```text
cargo test -p cosmon-session-probe --test notarisable_reference
```

Sur les quatre fixtures provider réellement présentes (deux Claude, deux
Codex), elle établit :

1. copie sous un autre chemin : `Verified` avec la même référence ;
2. rotation avec ancien fichier conservé : nouveau chemin `Mismatch`, ancien
   fichier renommé `Verified` ;
3. troncature : `Mismatch`, suffixe perdu non récupérable ;
4. effacement puis désérialisation après reprise : identifiant identique,
   résolution `Missing`.

La mission nommait aussi des fixtures **Cursor editor** « existantes ». Le
répertoire M1 n’en contient aucune et aucun `CursorProbe` n’existe : `cursor.rs`
est le curseur d’octets commun à Claude/Codex, pas un adaptateur Cursor editor.
Le test d’inventaire enregistre ce fait au lieu de fabriquer une fixture qui
aurait fait passer la prémisse par construction. Comme l’identité porte sur les
octets bruts, le résultat est indépendant du format provider ; la matrice
Cursor editor reste néanmoins non exécutée jusqu’à l’arrivée d’une fixture
synthétique et d’un adaptateur réels.

## Réutilisation des briques existantes

- ADR-011 : identité = contenu, emplacement = routage ;
- ADR-030 / `cosmon-archive` : modèle de vérification et chaîne, mais pas copie
  implicite des logs dans ce lot ;
- ADR-056 / `cosmon-notary` : commitment et signature Ed25519, sans prétendre
  que la signature assure disponibilité ou cognition ;
- `cs journal end` : cohérence avec le scellement BLAKE3 existant, avec une
  séparation de domaine propre au segment.
