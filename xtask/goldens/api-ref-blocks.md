===== routes-v1 =====
**42 routes `/v1/` gelées** — recomptées depuis le canon à chaque génération (`crates/cosmon-rpp-adapter/data/surface_events.txt`, cosmon). La colonne *Effet* est dérivée du scope requis (godel C5 : un scope distinct par effet coûteux ou irréversible, ADR-080 §6.5) — jamais éditée à la main.

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
| 11 | molecule | POST | `/v1/molecules/{id}/done` | `cosmon:molecule:write` | tenant-verb |  |
| 12 | molecule | GET | `/v1/molecules/{id}/session` | `cosmon:logs:subscribe` | adapter-only |  |
| 13 | molecule | GET | `/v1/molecules/{id}/status` | `cosmon:molecule:read` | tenant-verb |  |
| 14 | artifact | GET | `/v1/molecules/{id}/artifacts` | `cosmon:artifact:read` | adapter-only |  |
| 15 | artifact | GET | `/v1/molecules/{id}/artifacts/{token}` | `cosmon:artifact:read` | adapter-only |  |
| 16 | artifact | PUT | `/v1/molecules/{id}/artifacts/{token}` | `cosmon:artifact:write` | adapter-only |  |
| 17 | auth-claude | POST | `/v1/auth/claude/start` | — | adapter-only |  |
| 18 | auth-claude | POST | `/v1/auth/claude/email` | — | adapter-only |  |
| 19 | auth-claude | GET | `/v1/auth/claude/{session_id}` | — | adapter-only |  |
| 20 | auth-claude | DELETE | `/v1/auth/claude/{session_id}` | — | adapter-only |  |
| 21 | auth-claude | POST | `/v1/auth/claude/confirm` | — | adapter-only |  |
| 22 | observ. | GET | `/v1/auth/me` | — | adapter-only |  |
| 23 | observ. | GET | `/v1/events` | `cosmon:events:subscribe` | adapter-only |  |
| 24 | observ. | GET | `/v1/molecules/{id}/logs` | `cosmon:logs:subscribe` | adapter-only |  |
| 25 | observ. | GET | `/v1/quota` | `cosmon:molecule:read` | adapter-only |  |
| 26 | observ. | GET | `/v1/noyaux` | — | adapter-only |  |
| 27 | observ. | GET | `/v1/workers` | `cosmon:worker:read` | adapter-only |  |
| 28 | avatar-canal | POST | `/v1/avatar/converse` | `cosmon:pilote:converse` | tenant-verb |  |
| 29 | avatar-canal | POST | `/v1/avatar/perceive` | `cosmon:world:observe` | adapter-only |  |
| 30 | avatar-life | GET | `/v1/avatar/{instance_id}/status` | `cosmon:world:observe` | tenant-verb |  |
| 31 | avatar-life | POST | `/v1/avatar/{instance_id}/incarnate` | `cosmon:pilote:converse` | tenant-verb |  |
| 32 | avatar-life | POST | `/v1/avatar/{instance_id}/grant` | `cosmon:pilote:converse` | tenant-verb |  |
| 33 | avatar-life | GET | `/v1/avatar/{instance_id}/audit` | `cosmon:world:observe` | tenant-verb |  |
| 34 | avatar-life | GET | `/v1/avatar/{instance_id}/mould-info` | `cosmon:world:observe` | tenant-verb |  |
| 35 | admin | POST | `/v1/admin/habilitations` | — | adapter-only |  |
| 36 | admin | GET | `/v1/admin/habilitations` | — | adapter-only |  |
| 37 | admin | DELETE | `/v1/admin/habilitations/{id}` | — | adapter-only |  |
| 38 | admin | POST | `/v1/admin/reload` | — | adapter-only |  |
| 39 | admin | POST | `/v1/admin/federations` | — | adapter-only |  |
| 40 | admin | GET | `/v1/admin/federations` | — | adapter-only |  |
| 41 | admin | DELETE | `/v1/admin/federations/{id}` | — | adapter-only |  |
| 42 | admin | DELETE | `/v1/admin/federations/{id}/galaxies/{galaxy}` | — | adapter-only |  |

Découpage : **13** molecule + **3** artifact + **5** auth-claude + **6** observ. + **2** avatar-canal + **5** avatar-life + **8** admin = **42**.

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
| `POST /v1/molecules/{id}/done` | ✅ liée (verbe tenant, bijection testée) |
| `GET /v1/molecules/{id}/session` | ⊘ exempte (adapter-only) |
| `GET /v1/molecules/{id}/status` | ✅ liée (verbe tenant, bijection testée) |
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

Bijection liée : **17** (11 molecule + 1 avatar-canal + 5 avatar-life). Exemptes : **25**. Total : **42** — recompté depuis le canon (colonne `exposure`) à chaque génération.

