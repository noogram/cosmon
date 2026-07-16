===== routes-v1 =====
**39 routes `/v1/` gelées** — recomptées depuis le canon à chaque génération (`crates/cosmon-rpp-adapter/data/surface_events.txt`, cosmon). La colonne *Effet* est dérivée du scope requis (godel C5 : un scope distinct par effet coûteux ou irréversible, ADR-080 §6.5) — jamais éditée à la main.

| # | Famille | Méthode | Path | Scope requis | §8p | Effet |
|---|---|---|---|---|---|---|
| 1 | molecule | GET | `/v1/molecules` | `cosmon:molecule:read` | tenant-verb |  |
| 2 | molecule | GET | `/v1/molecules/{id}` | `cosmon:molecule:read` | tenant-verb |  |
| 3 | molecule | POST | `/v1/molecules` | `cosmon:molecule:write` | tenant-verb |  |
| 4 | molecule | POST | `/v1/molecules/{id}/tags` | `cosmon:molecule:write` | tenant-verb |  |
| 5 | molecule | POST | `/v1/molecules/{id}/collapse` | `cosmon:molecule:write` | tenant-verb |  |
| 6 | molecule | POST | `/v1/molecules/{id}/freeze` | `cosmon:molecule:write` | tenant-verb |  |
| 7 | molecule | POST | `/v1/molecules/{id}/stuck` | `cosmon:molecule:write` | tenant-verb |  |
| 8 | molecule | POST | `/v1/molecules/{id}/tackle` | `cosmon:molecule:write` **ET** `cosmon:worker:spawn` | tenant-verb | `[coûteux]` |
| 9 | molecule | GET | `/v1/molecules/{id}/result` | `cosmon:molecule:read` | adapter-only |  |
| 10 | molecule | POST | `/v1/molecules/{id}/run` | `cosmon:molecule:write` **ET** `cosmon:worker:spawn` | tenant-verb | `[coûteux]` |
| 11 | artifact | GET | `/v1/molecules/{id}/artifacts` | `cosmon:artifact:read` | adapter-only |  |
| 12 | artifact | GET | `/v1/molecules/{id}/artifacts/{token}` | `cosmon:artifact:read` | adapter-only |  |
| 13 | artifact | PUT | `/v1/molecules/{id}/artifacts/{token}` | `cosmon:artifact:write` | adapter-only |  |
| 14 | auth-claude | POST | `/v1/auth/claude/start` | — | adapter-only |  |
| 15 | auth-claude | POST | `/v1/auth/claude/email` | — | adapter-only |  |
| 16 | auth-claude | GET | `/v1/auth/claude/{session_id}` | — | adapter-only |  |
| 17 | auth-claude | DELETE | `/v1/auth/claude/{session_id}` | — | adapter-only |  |
| 18 | auth-claude | POST | `/v1/auth/claude/confirm` | — | adapter-only |  |
| 19 | observ. | GET | `/v1/auth/me` | — | adapter-only |  |
| 20 | observ. | GET | `/v1/events` | `cosmon:events:subscribe` | adapter-only |  |
| 21 | observ. | GET | `/v1/molecules/{id}/logs` | `cosmon:logs:subscribe` | adapter-only |  |
| 22 | observ. | GET | `/v1/quota` | `cosmon:molecule:read` | adapter-only |  |
| 23 | observ. | GET | `/v1/noyaux` | — | adapter-only |  |
| 24 | observ. | GET | `/v1/workers` | `cosmon:worker:read` | adapter-only |  |
| 25 | avatar-canal | POST | `/v1/avatar/converse` | `cosmon:pilote:converse` | tenant-verb |  |
| 26 | avatar-canal | POST | `/v1/avatar/perceive` | `cosmon:world:observe` | adapter-only |  |
| 27 | avatar-life | GET | `/v1/avatar/{instance_id}/status` | `cosmon:world:observe` | tenant-verb |  |
| 28 | avatar-life | POST | `/v1/avatar/{instance_id}/incarnate` | `cosmon:pilote:converse` | tenant-verb |  |
| 29 | avatar-life | POST | `/v1/avatar/{instance_id}/grant` | `cosmon:pilote:converse` | tenant-verb |  |
| 30 | avatar-life | GET | `/v1/avatar/{instance_id}/audit` | `cosmon:world:observe` | tenant-verb |  |
| 31 | avatar-life | GET | `/v1/avatar/{instance_id}/mould-info` | `cosmon:world:observe` | tenant-verb |  |
| 32 | admin | POST | `/v1/admin/habilitations` | — | adapter-only |  |
| 33 | admin | GET | `/v1/admin/habilitations` | — | adapter-only |  |
| 34 | admin | DELETE | `/v1/admin/habilitations/{id}` | — | adapter-only |  |
| 35 | admin | POST | `/v1/admin/reload` | — | adapter-only |  |
| 36 | admin | POST | `/v1/admin/federations` | — | adapter-only |  |
| 37 | admin | GET | `/v1/admin/federations` | — | adapter-only |  |
| 38 | admin | DELETE | `/v1/admin/federations/{id}` | — | adapter-only |  |
| 39 | admin | DELETE | `/v1/admin/federations/{id}/galaxies/{galaxy}` | — | adapter-only |  |

Découpage : **10** molecule + **3** artifact + **5** auth-claude + **6** observ. + **2** avatar-canal + **5** avatar-life + **8** admin = **39**.

===== bijection-8p =====
| Route | Statut bijection (§8p) |
|---|---|
| `GET /v1/molecules` | ✅ liée (verbe tenant, bijection testée) |
| `GET /v1/molecules/{id}` | ✅ liée (verbe tenant, bijection testée) |
| `POST /v1/molecules` | ✅ liée (verbe tenant, bijection testée) |
| `POST /v1/molecules/{id}/tags` | ✅ liée (verbe tenant, bijection testée) |
| `POST /v1/molecules/{id}/collapse` | ✅ liée (verbe tenant, bijection testée) |
| `POST /v1/molecules/{id}/freeze` | ✅ liée (verbe tenant, bijection testée) |
| `POST /v1/molecules/{id}/stuck` | ✅ liée (verbe tenant, bijection testée) |
| `POST /v1/molecules/{id}/tackle` | ✅ liée (verbe tenant, bijection testée) |
| `GET /v1/molecules/{id}/result` | ⊘ exempte (adapter-only) |
| `POST /v1/molecules/{id}/run` | ✅ liée (verbe tenant, bijection testée) |
| `GET /v1/molecules/{id}/artifacts` | ⊘ exempte (adapter-only) |
| `GET /v1/molecules/{id}/artifacts/{token}` | ⊘ exempte (adapter-only) |
| `PUT /v1/molecules/{id}/artifacts/{token}` | ⊘ exempte (adapter-only) |
| `POST /v1/auth/claude/start` | ⊘ exempte (adapter-only) |
| `POST /v1/auth/claude/email` | ⊘ exempte (adapter-only) |
| `GET /v1/auth/claude/{session_id}` | ⊘ exempte (adapter-only) |
| `DELETE /v1/auth/claude/{session_id}` | ⊘ exempte (adapter-only) |
| `POST /v1/auth/claude/confirm` | ⊘ exempte (adapter-only) |
| `GET /v1/auth/me` | ⊘ exempte (adapter-only) |
| `GET /v1/events` | ⊘ exempte (adapter-only) |
| `GET /v1/molecules/{id}/logs` | ⊘ exempte (adapter-only) |
| `GET /v1/quota` | ⊘ exempte (adapter-only) |
| `GET /v1/noyaux` | ⊘ exempte (adapter-only) |
| `GET /v1/workers` | ⊘ exempte (adapter-only) |
| `POST /v1/avatar/converse` | ✅ liée (verbe tenant, bijection testée) |
| `POST /v1/avatar/perceive` | ⊘ exempte (adapter-only) |
| `GET /v1/avatar/{instance_id}/status` | ✅ liée (verbe tenant, bijection testée) |
| `POST /v1/avatar/{instance_id}/incarnate` | ✅ liée (verbe tenant, bijection testée) |
| `POST /v1/avatar/{instance_id}/grant` | ✅ liée (verbe tenant, bijection testée) |
| `GET /v1/avatar/{instance_id}/audit` | ✅ liée (verbe tenant, bijection testée) |
| `GET /v1/avatar/{instance_id}/mould-info` | ✅ liée (verbe tenant, bijection testée) |
| `POST /v1/admin/habilitations` | ⊘ exempte (adapter-only) |
| `GET /v1/admin/habilitations` | ⊘ exempte (adapter-only) |
| `DELETE /v1/admin/habilitations/{id}` | ⊘ exempte (adapter-only) |
| `POST /v1/admin/reload` | ⊘ exempte (adapter-only) |
| `POST /v1/admin/federations` | ⊘ exempte (adapter-only) |
| `GET /v1/admin/federations` | ⊘ exempte (adapter-only) |
| `DELETE /v1/admin/federations/{id}` | ⊘ exempte (adapter-only) |
| `DELETE /v1/admin/federations/{id}/galaxies/{galaxy}` | ⊘ exempte (adapter-only) |

Bijection liée : **15** (9 molecule + 1 avatar-canal + 5 avatar-life). Exemptes : **24**. Total : **39** — recompté depuis le canon (colonne `exposure`) à chaque génération.

