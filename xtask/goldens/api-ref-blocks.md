===== routes-v1 =====
**62 routes `/v1/` gelées** — recomptées depuis le canon à chaque génération (`crates/cosmon-rpp-adapter/data/surface_events.txt`, cosmon). La colonne *Effet* est dérivée du scope requis (godel C5 : un scope distinct par effet coûteux ou irréversible, ADR-080 §6.5) — jamais éditée à la main.

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
| 14 | molecule | PUT | `/v1/molecules/{id}/collapse-receipts/{key}` | `cosmon:molecule:write` | tenant-verb |  |
| 15 | molecule | GET | `/v1/molecules/{id}/collapse-receipts/{key}` | `cosmon:molecule:read` | tenant-verb |  |
| 16 | molecule | POST | `/v1/molecules/{id}/evolve` | `cosmon:molecule:write` | tenant-verb |  |
| 17 | molecule | POST | `/v1/molecules/{id}/resurrect` | `cosmon:molecule:write` **ET** `cosmon:worker:spawn` | tenant-verb | `[coûteux]` |
| 18 | molecule | GET | `/v1/molecules/{id}/deps` | `cosmon:molecule:read` | tenant-verb |  |
| 19 | molecule | GET | `/v1/molecules/{id}/prime` | `cosmon:molecule:read` | tenant-verb |  |
| 20 | molecule | POST | `/v1/molecules/{id}/decay` | `cosmon:molecule:write` | tenant-verb |  |
| 21 | molecule | POST | `/v1/molecules/merge` | `cosmon:molecule:write` | tenant-verb |  |
| 22 | molecule | POST | `/v1/molecules/{id}/transform` | `cosmon:molecule:write` | tenant-verb |  |
| 23 | artifact | GET | `/v1/molecules/{id}/artifacts` | `cosmon:artifact:read` | adapter-only |  |
| 24 | artifact | GET | `/v1/molecules/{id}/artifacts/{token}` | `cosmon:artifact:read` | adapter-only |  |
| 25 | artifact | PUT | `/v1/molecules/{id}/artifacts/{token}` | `cosmon:artifact:write` | adapter-only |  |
| 26 | harvest | GET | `/v1/harvest/status` | `cosmon:molecule:read` | adapter-only |  |
| 27 | harvest | POST | `/v1/harvest/challenge` | `cosmon:molecule:harvest` | adapter-only |  |
| 28 | harvest | POST | `/v1/harvest/grants` | `cosmon:molecule:harvest` | adapter-only |  |
| 29 | auth-claude | POST | `/v1/auth/claude/start` | — | adapter-only |  |
| 30 | auth-claude | POST | `/v1/auth/claude/email` | — | adapter-only |  |
| 31 | auth-claude | GET | `/v1/auth/claude/{session_id}` | — | adapter-only |  |
| 32 | auth-claude | DELETE | `/v1/auth/claude/{session_id}` | — | adapter-only |  |
| 33 | auth-claude | POST | `/v1/auth/claude/confirm` | — | adapter-only |  |
| 34 | observ. | GET | `/v1/auth/me` | — | adapter-only |  |
| 35 | observ. | GET | `/v1/events` | `cosmon:events:subscribe` | adapter-only |  |
| 36 | observ. | GET | `/v1/molecules/{id}/logs` | `cosmon:logs:subscribe` | adapter-only |  |
| 37 | observ. | GET | `/v1/quota` | `cosmon:molecule:read` | adapter-only |  |
| 38 | observ. | GET | `/v1/noyaux` | — | adapter-only |  |
| 39 | observ. | GET | `/v1/workers` | `cosmon:worker:read` | adapter-only |  |
| 40 | observ. | GET | `/v1/vitals` | `cosmon:molecule:read` | adapter-only |  |
| 41 | observ. | GET | `/v1/ledger` | `cosmon:events:subscribe` **ET** `cosmon:molecule:read` | adapter-only |  |
| 42 | avatar-canal | POST | `/v1/avatar/converse` | `cosmon:pilote:converse` | tenant-verb |  |
| 43 | avatar-canal | POST | `/v1/avatar/perceive` | `cosmon:world:observe` | adapter-only |  |
| 44 | avatar-life | GET | `/v1/avatar/{instance_id}/status` | `cosmon:world:observe` | tenant-verb |  |
| 45 | avatar-life | POST | `/v1/avatar/{instance_id}/incarnate` | `cosmon:pilote:converse` | tenant-verb |  |
| 46 | avatar-life | POST | `/v1/avatar/{instance_id}/grant` | `cosmon:pilote:converse` | tenant-verb |  |
| 47 | avatar-life | GET | `/v1/avatar/{instance_id}/audit` | `cosmon:world:observe` | tenant-verb |  |
| 48 | avatar-life | GET | `/v1/avatar/{instance_id}/mould-info` | `cosmon:world:observe` | tenant-verb |  |
| 49 | admin | POST | `/v1/admin/habilitations` | — | adapter-only |  |
| 50 | admin | GET | `/v1/admin/habilitations` | — | adapter-only |  |
| 51 | admin | DELETE | `/v1/admin/habilitations/{id}` | — | adapter-only |  |
| 52 | admin | POST | `/v1/admin/reload` | — | adapter-only |  |
| 53 | admin | POST | `/v1/admin/federations` | — | adapter-only |  |
| 54 | admin | GET | `/v1/admin/federations` | — | adapter-only |  |
| 55 | admin | DELETE | `/v1/admin/federations/{id}` | — | adapter-only |  |
| 56 | admin | DELETE | `/v1/admin/federations/{id}/galaxies/{galaxy}` | — | adapter-only |  |
| 57 | admin | PUT | `/v1/admin/noyaux/{noyau}/harvest-authority` | — | adapter-only |  |
| 58 | work | GET | `/v1/work/{owner}` | `cosmon:work:read` | adapter-only |  |
| 59 | work | POST | `/v1/work/{owner}/messages` | `cosmon:work:write` | adapter-only |  |
| 60 | work | GET | `/v1/work/{owner}/inbox` | `cosmon:work:read` | adapter-only |  |
| 61 | work | POST | `/v1/work/{owner}/inbox` | `cosmon:work:read` | adapter-only |  |
| 62 | work | POST | `/v1/work/{owner}/messages/{key}/ack` | `cosmon:work:write` | adapter-only |  |

Découpage : **22** molecule + **3** artifact + **3** harvest + **5** auth-claude + **8** observ. + **2** avatar-canal + **5** avatar-life + **9** admin + **5** work = **62**.

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
| `PUT /v1/molecules/{id}/collapse-receipts/{key}` | ✅ liée (verbe tenant, bijection testée) |
| `GET /v1/molecules/{id}/collapse-receipts/{key}` | ✅ liée (verbe tenant, bijection testée) |
| `POST /v1/molecules/{id}/evolve` | ✅ liée (verbe tenant, bijection testée) |
| `POST /v1/molecules/{id}/resurrect` | ✅ liée (verbe tenant, bijection testée) |
| `GET /v1/molecules/{id}/deps` | ✅ liée (verbe tenant, bijection testée) |
| `GET /v1/molecules/{id}/prime` | ✅ liée (verbe tenant, bijection testée) |
| `POST /v1/molecules/{id}/decay` | ✅ liée (verbe tenant, bijection testée) |
| `POST /v1/molecules/merge` | ✅ liée (verbe tenant, bijection testée) |
| `POST /v1/molecules/{id}/transform` | ✅ liée (verbe tenant, bijection testée) |
| `GET /v1/molecules/{id}/artifacts` | ⊘ exempte (adapter-only) |
| `GET /v1/molecules/{id}/artifacts/{token}` | ⊘ exempte (adapter-only) |
| `PUT /v1/molecules/{id}/artifacts/{token}` | ⊘ exempte (adapter-only) |
| `GET /v1/harvest/status` | ⊘ exempte (adapter-only) |
| `POST /v1/harvest/challenge` | ⊘ exempte (adapter-only) |
| `POST /v1/harvest/grants` | ⊘ exempte (adapter-only) |
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
| `GET /v1/vitals` | ⊘ exempte (adapter-only) |
| `GET /v1/ledger` | ⊘ exempte (adapter-only) |
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
| `PUT /v1/admin/noyaux/{noyau}/harvest-authority` | ⊘ exempte (adapter-only) |
| `GET /v1/work/{owner}` | ⊘ exempte (adapter-only) |
| `POST /v1/work/{owner}/messages` | ⊘ exempte (adapter-only) |
| `GET /v1/work/{owner}/inbox` | ⊘ exempte (adapter-only) |
| `POST /v1/work/{owner}/inbox` | ⊘ exempte (adapter-only) |
| `POST /v1/work/{owner}/messages/{key}/ack` | ⊘ exempte (adapter-only) |

Bijection liée : **26** (20 molecule + 1 avatar-canal + 5 avatar-life). Exemptes : **36**. Total : **62** — recompté depuis le canon (colonne `exposure`) à chaque génération.

