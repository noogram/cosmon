# sensorium smoke fixture

Hand-shaped fixture for `tests/sensorium_strip.rs` and any future
golden-snapshot test that wants a stable five-organ aggregate without
relying on the wall clock. The integration test generates a richer
fixture programmatically (so timestamps stay within the 24h / 6h
windows); this directory is the **byte-static** version frozen for
documentation purposes.

```
sensorium/
├── inbox.ndjson            # peau: one row per landed signal
├── heartbeat.ndjson        # cœur: ten beats, one live
├── cosmon/SOUL.md          # visage: identity (frontmatter `name:`)
├── notes/*.md              # carnet: durable notes
└── outbox/*.md             # voix: drafts with `permission: ...`
```

Per `ADR-NEXT-sensorium-strip`, every file is optional and missing
files collapse to the zero baseline; this fixture exercises *every*
organ at once so a single load round-trip can validate the full
parser surface.
