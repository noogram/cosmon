# Raw outputs — issue-#20 door-4 differential replay

The report that reads these files is
[`../issue-20-door-4-differential.md`](../issue-20-door-4-differential.md).
Produced by `scripts/container-worker-doors-differential.sh`.

> **Engine, corrected 2026-07-27.** Every `environment.txt` here records
> `docker_context desktop-linux` — Docker Desktop 27.3.1, kernel
> `6.10.11-linuxkit`. That is what these runs were taken on and the files are
> published unchanged. What was wrong is the belief that this was the external
> reporter's engine: he corrected his own description on 2026-07-27 and named
> Colima / Ubuntu 24.04.4 LTS / aarch64. See
> [`../engine-fidelity-2026-07-27.md`](../engine-fidelity-2026-07-27.md) for
> the measured posture of both engines and what does and does not follow. Runs
> made after that date are taken on `colima-cosmon-bench`; read the engine off
> the file rather than assuming which one produced it.

Each run is one complete differential: the **same** harness
(`docker/container-worker-doors/in-container-bench.sh`, SHA-256
`c9808df181dfaa5a25c27aac8513faa75c9d055d33e2585dc5f5e320b3ac12fc`) executed
against two builds of `cs` — `4c41738`, the parent of the door-4 fix, and
`73c4b2a`, the fix. Nothing else differs between the two passes.

| file | what it is |
|---|---|
| `environment.txt` | host, engine, harness hash, the two commits — recorded once and shared by both passes |
| `provenance-parent.txt` / `provenance-fixed.txt` | image identity, base-image digests, the harness hash read back **out of the image**, and the four binary provenance markers |
| `bench-parent.log` / `bench-fixed.log` | the bench's complete output, unsummarised |
| `bench-*.ansi.log` | the same bytes with the terminal colour escapes still in |
| `verdicts-*.txt` | just the `VERDICT` lines |
| `build-*.excerpt.log` | the base-image digests and stage boundaries from the docker build |

The `.log` files differ from the `.ansi.log` files by ANSI colour escapes
only. No line was removed, reordered, condensed or reworded.

`run-1` was produced by the first version of the driver, which verified the
harness hash on disk; `run-2` and `run-3` by the version that also reads the
hash back out of the built image and records the base-image digests. Run 1's
outputs are published as they were produced rather than re-issued under the
newer driver.

All three runs returned the same verdicts: arm C **NOT PROVEN** on `4c41738`,
**PROVEN** on `73c4b2a`, arms A/B/D/E identical on both heads.

## No secrets

The only credential-shaped string anywhere in these files is the literal
`PLACEHOLDER-NOT-A-CREDENTIAL-cosmon-bench-issue-20`, minted inside the
container by the bench. It authenticates nothing; door 3 `stat`s the file and
never reads it. No real credential was created, requested, copied, read or
logged, and no host credential was mounted.
