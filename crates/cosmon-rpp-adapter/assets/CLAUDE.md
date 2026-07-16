## cosmon-remote — ton instance d'agents distante

Tu pilotes une instance cosmon par UNE seule fente : le binaire `cosmon-remote` (alias `cosmon`). Jamais `ssh`, jamais `docker exec` — tout passe par lui.
- Découvrir : `cosmon-remote --help`, puis `cosmon-remote <famille> --help`, ou `man cosmon-remote`. Le help EST la référence — ne recopie aucun catalogue de routes.
- Démarrer (deux badges, dans l'ordre) : ton badge tenant vient du profil posé à l'install ; `cosmon-remote auth login --email <toi>` connecte le worker Claude (une fois). `cosmon-remote doctor` vérifie les deux.
- Travailler : `molecule nucleate` crée le casier → `molecule tackle <id>` lance l'agent → `molecule result <id>` imprime le livrable. `molecule list` pour l'état.
- Coût : seul `tackle` brûle du crédit. Les lectures (`list`, `get`, `result`, `events`, `quota`) sont bon marché.
- Quotas et limites : lis l'oracle vivant (`cosmon-remote quota`), jamais une valeur mémorisée.
- Interruption : toi tu peux couper l'instance (`molecule collapse`/`freeze`) ; elle ne t'interrompt jamais sans permission explicite de ta part.
- Les verbes opérateur (`done`, `kill`, `evolve`, `run`…) n'existent pas côté client — c'est un refus voulu (§8p), ne les cherche pas.
