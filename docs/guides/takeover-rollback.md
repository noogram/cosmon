<!-- SPDX-License-Identifier: AGPL-3.0-only -->

# Handing the controls over — and taking them back

Your primary pilot has stopped. Claude Code hit its weekly limit mid-mission,
or the machine it was on went to sleep, or you simply want the other model to
finish the job. The fleet is fine. The molecules are fine. What you have lost
is the cockpit.

This guide is the procedure that gets it back, and — the half most handover
documents forget — the procedure that undoes it when the handover was the
wrong call.

It assumes ADR-168 (the co-pilotage substrate) and ADR-171 (the operator
gesture is a signature). You do not have to have read them. You do have to
have done the one-time setup in §1, and you have to do it *before* the day you
need this page, because §1 is the only step in this guide that needs a pilot
who still works.

---

## 1. Once, before you need it — pin the trust root

A grant is honoured only if it carries a signature from a key cosmon has been
told to trust. **No pinned key means no grant is honoured** — not "grants are
unverified", but "the transfer refuses". That direction is deliberate: if a
missing key meant *unverified*, an agent that wanted the controls would only
have to delete the key.

```console
$ minisign -G -p ~/galaxies/<galaxy>/.cosmon/takeover.pub \
           -s ~/.minisign/cosmon-takeover.key
$ git -C ~/galaxies/<galaxy> add .cosmon/takeover.pub && git commit -m "chore: pin the takeover trust root"
```

Two things about where those files go.

The **public** key is committed, inside the galaxy and next to the
configuration rather than under `.cosmon/state/`. State is scratch nobody
re-reads; a committed trust root turns a key exchange from an invisible act
into a diff somebody reviews.

The **secret** key goes anywhere the agents you run are not reading, and it
keeps a passphrase. That passphrase is the whole mechanism. Everything else in
this file is bookkeeping around one fact: signing needs something that lives in
your head. An agent running under your UID reads every file you read and runs
every binary you run — so a check on *the shape of the caller* (is it a TTY? is
there a nonce file?) refuses an inattentive agent and not a determined one. A
check on *possession of a secret* is the only kind that survives.

Point cosmon at the key by committing it to `<galaxy>/.cosmon/takeover.pub`, or
by exporting `COSMON_TAKEOVER_PUBKEY=/path/to/takeover.pub` when it lives
elsewhere.

---

## 2. Before the handover — the checkpoint is the whole payload

A takeover restores a *cockpit*, not a conversation. What the successor
inherits is the checkpoint the predecessor published, and nothing else:

```console
$ cs sessions checkpoint publish --mission <mission-id> \
    --include "…what this mission covers…" \
    --hypothesis "subject:affirm=…what I currently believe…" \
    --next "subject=…what I was about to do…" \
    --did "…what is already done…" \
    --risk "…what could still bite…" \
    --question "…what I could not settle…" \
    --evidence "subject=path/to/file#L120"
```

Publish one at every natural transition, not only when you smell a limit
coming. A pilot that hits its ceiling gets no warning — the API returns an
error, it does not send a courtesy note ten minutes ahead (ADR-168 D3.1). The
checkpoint you will hand over is the last one you happened to write, so the
useful cadence is "after each decision", not "before each disaster".

The measure of a good checkpoint is blunt: **the successor should never need
the predecessor's transcript.** If it does, that is falsifier 9 of ADR-168 and
the checkpoint was too thin. §5 of this guide gives the four fields that were
actually consulted during the M8 exercise, in the order they were consulted.

---

## 3. The transfer, in four commands

Three actors, and it matters which is which: the **outgoing pilot** (which may
be unable to type anything at all — that is the whole point), the **incoming
pilot**, and **you**. Only the third one can grant.

**a. The incoming pilot asks.** This confers nothing; it writes an ask into the
ledger so you have something to answer.

```console
$ cs sessions takeover request --mission <mission-id> --to <incoming-sid> \
    --reason "primary hit its weekly limit at 14:31Z"
```

A pilot may ask. A quota reading may not ask on its behalf, and neither may a
heartbeat gap: there is no automation here to disable, because none was built
(TAKEOVER-SUPERVISED).

**b. You read the challenge.** This is the text you are about to sign, and you
should read it as a sentence rather than as a blob — it names the mission, the
session that will be seated, and the epoch:

```console
$ cs sessions takeover challenge --mission <mission-id> --request <request-id> > /tmp/challenge
$ cat /tmp/challenge
cosmon-takeover-grant-v1
mission=task-20260804-da48
holder=codex-m8-primary
epoch=2
granted_by=emmanuel
ttl=none
```

The `epoch=` line is what makes a captured signature useless twice: epochs are
strictly increasing per mission, so a signature for epoch 2 authorises epoch 2
and never epoch 3. Replay protection falls out of the same arithmetic that
prevents split-brain — no nonce store, no clock to trust.

**c. You sign it.** Out of band, with the tool, with the passphrase:

```console
$ minisign -S -s ~/.minisign/cosmon-takeover.key -m /tmp/challenge -x /tmp/grant.minisig
```

There is deliberately no `cs … sign`. A signing verb would be a verb the
beneficiary could call, and the mechanism *is* its absence: nothing in the
shipped tree can sign a takeover challenge.

**d. The grant is recorded.**

```console
$ cs sessions takeover grant --mission <mission-id> --request <request-id> \
    --attestation /tmp/grant.minisig
task-20260804-da48: codex-m8-primary is PRIMARY at epoch 2 — every earlier epoch is refused
```

Anybody may *type* that command — it is the signature that authorises, not the
typist. And the verification happens again at every read of the ledger, not
only here: an agent that appends a forged line to `pilot-lease/<mission>.grants.jsonl`
by hand has written a line that every reader skips.

**e. The incoming pilot takes the seat**, presenting the epoch it now believes
it holds:

```console
$ cs sessions attach --role primary --session <incoming-sid> \
    --as <provider>:<native-session-id> --mission <mission-id> --epoch 2
$ cs tackle <mission-id> --dry-run   # or any lifecycle verb — it now passes the guard
```

The outgoing pilot needs to do nothing to step down, and this is the part worth
trusting: its next ordinary heartbeat reads the ledger, finds an epoch above
its own, and demotes itself to co-pilot. A pilot that is asleep, rate-limited
or dead steps down just as reliably as one that is paying attention, because
stepping down is not something it does — it is something the ledger says about
it.

---

## 4. What refuses, and what each refusal means

Every one of these exits non-zero. A refusal from the guard on a lifecycle verb
exits **16**, which is its own code because its remedy is its own: not a
redispatch, not a repair, but a grant that only a human can issue.

| What you did | What you get | What it means |
|---|---|---|
| A gesture from the pilot that used to hold the lease | `refused: … the lease is held by <other>` | It missed the transfer. Nothing was mutated — the refusal is *before* the effect. |
| A gesture presenting an epoch below the head | names both generations | The stale-epoch falsifier. Identity alone would have let it through; the epoch is what stopped it. |
| A gesture from a session with no presence snapshot | `refused: … no epoch presented` | A claim that names no generation is not a claim. |
| A grant with no `--attestation` | the flag is required | `--by` is a label, the signature is the gesture. |
| A grant signed by the wrong key | the ledger line is skipped on read | A grant that did not happen. |
| Any of the above with no pinned trust root | refusal | Deleting the key stops handovers; it does not open them. |

If you are scripting against these, note that they are not all one shape: four
render as a `refused: …` line and the malformed-epoch case renders as a typed
error. That was friction F5 of the M7 dogfood and it is still true.

---

## 5. Taking the controls back is another handover

Do not edit the old grant out of the ledger. Do not try to put the epoch back
to the number it had before. Both moves make the thing you need to understand
later — that the wrong seat existed — harder to see, and neither makes a stale
pilot safe. A rollback is a new, signed handover to the pilot who should have
the controls now.

There are three ordinary reasons to do it: the wrong successor was seated; the
predecessor came back with the context that matters; or the successor is making
the mission worse. They differ in urgency, not in mechanics. Ask for the seat
that should exist after the correction, read and sign the next challenge, then
record the grant:

```console
$ cs sessions takeover request --mission <mission-id> --to <returning-sid> \
    --reason "returning controls after an incorrect handover"
$ cs sessions takeover challenge --mission <mission-id> --request <request-id> \
    > /tmp/rollback-challenge
$ cat /tmp/rollback-challenge
$ minisign -S -s ~/.minisign/cosmon-takeover.key \
    -m /tmp/rollback-challenge -x /tmp/rollback.minisig
$ cs sessions takeover grant --mission <mission-id> --request <request-id> \
    --attestation /tmp/rollback.minisig
```

The challenge names the next epoch. If the mistaken transfer was epoch 2, the
correction is epoch 3 — even when it returns the seat to the person who held
epoch 1. That is not a cosmetic counter. The old successor's next gesture
presents epoch 2 and is refused; the returned pilot must present epoch 3.

The rollback costs a real operator gesture: another challenge, another
passphrase prompt, and another ledger line. That friction is correct. The
operator is making a new decision in new circumstances, not pressing an undo
button an agent might learn to press for itself.

It does not undo work already done. The guard refuses a later `cs` gesture; it
does not reverse a molecule transition, a message, a merge, or a file the
wrongly seated pilot already changed. Stop to inspect those effects first when
that matters. Then use the new primary's checkpoint to decide what needs
repairing. The lease records who may make the next decision; it is not a time
machine.

## 6. The edge of the mechanism

The lease protects co-piloted mission gestures that go through `cs`. It does
not make a shell read-only. A pilot can still edit a file, run `git commit`,
send a network request, or invoke some other tool directly; no lease stands
between that process and a text editor. The boundary is honest because it is
small enough to audit: this mechanism decides who may fly the mission through
cosmon, not who may touch the machine.

That is why a clean rollback has two parts. First, hand the controls to the
right session at the next epoch so the next protected gesture is refused or
accepted correctly. Second, inspect the working tree, the history, and the
external systems the incorrect pilot could have touched. Do not report the
first part as if it had completed the second.

The same limit applies to the trust root. A process with write access to the
galaxy can replace `.cosmon/takeover.pub`; ADR-171 makes that swap visible in
the tracked file and in the key id recorded with a grant, but it cannot make
the host immutable. Keep the signing secret away from unattended processes,
review trust-root diffs, and use a hardware token or a second machine when the
remaining host-level risk is unacceptable.
