# ADR-171 — The operator gesture is a signature, not a string

**Status:** Accepted (2026-08-04).
**Date:** 2026-08-04.
**Decider:** Noogram.
**Authoring task:** `task-20260804-7779`.

**Entry artefact.** The M7 co-pilotage dogfood
(`task-20260731-bd92`, result §8, friction **F1**): `cs sessions takeover grant
--to X --by emmanuel` takes `--by` as a free string, and in the dogfood the
agent that typed the grant was the agent the grant seated.

**Related ADRs:**
[ADR-168](168-a-co-pilot-inherits-the-session-substrate-not-its-delivery-contract.md)
(§D3.1 and §D6 — the lease, the epoch, TAKEOVER-SUPERVISED),
[ADR-056](056-notary-protocol-v0.md) (the other operator key in this tree, and
why it is *not* this one),
[ADR-084](084-release-signing-cosign-piv.md) (the same shape at release scale:
a signature whose secret is not on the build host).

---

## Context

### The invariant rested on a convention

ADR-168 §D6 says the PRIMARY seat changes hands only by an operator gesture,
never by a pilot's own decision. Everything downstream of that sentence was
built carefully: epochs are strictly increasing so two pilots cannot both be at
the head; a gesture carries the epoch it believes it holds; anything unknown is
read-only; requests and grants live in separate append-only files with separate
writers.

The gesture itself was a string. `PilotLease::granted_by` held whatever the
caller typed after `--by`, and nothing in the system distinguished a human
typing `emmanuel` from an agent typing `emmanuel`.

In the M7 dogfood this was not hypothetical. The command that seated the
primary was run by the primary. Five authority falsifiers passed in that
exercise — a co-pilot could not pilot, a stale epoch was refused, an unnamed
epoch was refused, an unknown session was refused, the co-pilot could not take
the seat. All five test the *lease*. None tests the *grant*, because until now
there was nothing there to test.

A supervision that its beneficiary can forge is not supervision. So M8 — the
supervised relief exercise — could not honestly begin.

### What the adversary actually is

Not a remote attacker. A process running as the operator, on the operator's
host, holding the operator's files. It reads every secret on that disk, runs
every binary the operator can run, allocates a tty if one is demanded, sets any
environment variable, and appends any line to any file under `.cosmon/state/`.

Three candidate mechanisms were on the table when this molecule opened. Two do
not survive that adversary:

- **A tty / interactive-consent check.** Bypassed by `script`, `expect`, or any
  pty allocation. It refuses an *unattended* agent, not a determined one.
- **A single-use nonce the operator deposits.** The nonce file is on the same
  disk. Reading it is the whole attack.

Both share a defect: they test the *shape* of the caller rather than possession
of a secret. Against an adversary with the operator's own filesystem, only the
second kind of test survives.

The third — a detached signature under a key the agent does not hold —
survives, with one honest caveat kept in §Consequences.

---

## Decision

**A grant is honoured only if an operator signed it, and cosmon can check that
signature but cannot produce one.**

### D1 — What is signed

A `GrantChallenge`: the transfer itself, in canonical line-oriented text.

```text
cosmon-takeover-grant-v1
mission=task-20260731-9cf4
holder=claude:8ae462b2
epoch=2
granted_by=emmanuel
ttl=none
```

Every field that decides *who may fly and until when* is inside; nothing else
is. Three of those lines are load-bearing:

- `epoch` makes a captured signature single-use. Epochs are strictly increasing
  per mission, so a signature for epoch 2 authorises epoch 2 and never the
  next. **Replay protection falls out of the arithmetic that already prevents
  split-brain** — no nonce store, no clock.
- `granted_by` is the direct repair of F1. The operator name is now a claim the
  signature covers rather than a string the caller chose.
- The version tag on line 1 means a future v2 encoding shares no preimage with
  v1, so a v1 signature can never be replayed as a v2 grant.

`granted_by` rejects control characters: a newline there would let a caller
append lines of its own to the signed text.

### D2 — Cosmon verifies, and owns no stamp

There is **no signing path for the takeover key anywhere in the shipped tree**.
`cosmon-notary::minisign` parses and verifies; that is all it does. A `cs
sessions takeover sign` verb would be a verb the beneficiary could call, and the
mechanism is precisely the absence of one. A test asserts this property rather
than trusting it (`the_shipped_tree_owns_no_signing_path_for_the_operator_key`).

The operator signs out of band with stock `minisign`, whose secret key is
passphrase-protected by code cosmon does not have to get right. The passphrase
lives in a head — the one place on that host an agent cannot read.

Reusing minisign's on-disk format rather than inventing a cosmon one buys a
third thing beyond the tool and the encryption: a grant recorded in the ledger
stays verifiable with `minisign -V` by somebody who does not trust, or no
longer has, this codebase.

### D3 — The check is at read time, not only at write time

This is the part that makes it a mechanism instead of advice.

An agent that wants the seat does not have to go through `cs` at all. It can
`echo` a JSON line into `pilot-lease/<mission>.grants.jsonl`. A write-time check
in the grant command is therefore advice to well-behaved callers. Authority is
*derived from the ledger*, so the check lives where the derivation happens:
`PilotLeaseStore::grants` re-verifies every line each time it is read, and a
line whose signature does not check is skipped exactly like a torn one — a grant
that did not happen.

The write-time check is kept, for ergonomics only: it turns "your grant is
silently inert" into a message naming which field of the challenge the
signature fails to cover.

### D4 — Absence of a trust root refuses; it does not permit

With no operator public key pinned, **no grant is honoured at all**. The
alternative reading — "unverified when unconfigured" — would hand the
beneficiary a one-command bypass: delete the key, forge the grant. Here deleting
the key stops transfers instead of unlocking them, which is the fail-closed
shape `authorize` already has.

### D5 — Where the key is pinned

`$COSMON_TAKEOVER_PUBKEY` if set, else `<galaxy>/.cosmon/takeover.pub`.

Deliberately in the galaxy and outside `.cosmon/state/`. `state/` is runtime
scratch nobody reviews; a file beside the galaxy's configuration is one an
operator can **commit**, and a committed trust root turns a key swap from an
invisible act into a diff.

### D6 — A refused line is shown, never hidden

`cs sessions takeover show --history` prints refused lines with the reason
(`NOT AN OPERATOR GESTURE: …`). A forgery that vanishes from every view is a
forgery nobody investigates.

### D7 — Refused

1. **A tty or interactive-consent gate as the mechanism.** See §Context. It may
   still be added as a second, independent layer; it is not the load-bearing
   one and must never be described as if it were.
2. **A nonce file deposited by the operator.** Same disk, same reader.
3. **Signing with `~/.config/cosmon/operator.key`** (the ADR-056 notary key).
   That key is a plaintext hex file the agent can read, so signing with it
   would be theatre. The takeover key is a *different* key with a *different*
   custody requirement: it must never be usable by a process running on this
   host unattended.
4. **A `cs` verb that generates or uses the takeover secret.** D2.
5. **Falling back to unverified when the key file is missing.** D4.

---

## Consequences

### What is now true

- The gesture in the M7 dogfood — the agent granting itself the seat while
  naming a human — is refused, and refused whether it goes through `cs` or
  writes the ledger directly.
- A signature is bound to one mission, one holder, one epoch, one operator name
  and one ttl. Change any of them after the fact and it stops verifying.
- The ledger is independently auditable: each line carries the key id that
  signed it and enough material to reconstruct a `minisign`-checkable file.

### What is still assumed, stated plainly

Verification needs a trusted public key, and the file holding it sits on the
same disk as the agent. **An agent may overwrite it with its own.** This ADR
does not make that impossible.

What it does is make it *recorded and visible*: every grant carries the key id
that signed it, so a substituted trust root appears as a key change in an
append-only ledger even if the `.pub` file is restored afterwards; and pinning
the key in a tracked file makes the substitution a diff somebody reads.

So the honest claim is not "forgery is impossible". It is:

> Forging an operator gesture now requires either a secret the agent does not
> have, or a swap of the trust root that it cannot hide.

That is a real change in kind from a free string, and it is not the same as
unconditional unforgeability. Closing the remaining gap needs a root of trust
outside the host — a hardware token, a remote attestor, or a second machine —
and is deliberately out of scope here.

### Cost

- Every transfer is now a passphrase prompt. That is the intended friction: it
  is what makes the gesture a gesture. It was also, for one day, three commands
  and a temporary file — see the postscript.
- A galaxy with no pinned key cannot transfer a seat until it pins one.
  `cs sessions takeover trust` reports which key is in force, or that none is.
- M7 friction **F9** — the co-pilot's read-only role is requested rather than
  enforced — is *not* addressed here. It remains open for M8.

---

## Falsifiers

This ADR is wrong if any of the following turns out to be true. Each is checked
by `crates/cosmon-cli/tests/takeover_unforgeable.rs` unless noted.

1. An agent holding the state directory, the ledger file, the `cs` binary, the
   pinned public key and a shell can produce a lease the guard honours.
2. A ledger line appended without going through `cs` seats its holder.
3. A signature produced for one mission, holder, epoch, operator name or ttl
   authorises a different one.
4. Deleting the pinned public key makes grants pass rather than fail.
5. Some `cs` verb signs a takeover challenge.
6. A refused ledger line is invisible to `takeover show --history`.
7. `cosmon-notary::minisign` disagrees with stock `minisign` on a real
   artefact — pinned by a genuine `minisign 0.12` fixture in that module's
   tests, not by a self-generated one.

---

## Postscript, 2026-08-05 — `--sign-with` (`task-20260805-2b6d`)

**Entry artefact.** The operator, on reading the recipe M8-bis had to write out
line by line (`task-20260805-e77f` §6): *« la procédure me semble tout de même
hyper compliquée »*. Three commands, a temporary file, and a `--by` that had to
be repeated identically or the signature covered nothing (M8 friction **F12**).

The friction this ADR intended is the **passphrase**. Everything else that had
accreted around it — composing bytes into a file, remembering `.minisig`, typing
the same operator name twice — is not friction, it is clerical work, and
clerical work does not make a gesture more deliberate. Confusing the two is how
"it is secure" becomes an excuse for "it is painful".

So `cs sessions takeover grant … --sign-with <secret-key>` folds the three into
one: compute the challenge, **print it** for reading, run the operator's
`minisign(1)` as a child with the terminal attached, read back the signature,
delete both temporary files, append the grant.

**What is unchanged, and why this is not a stamp.** cosmon still contains no
signing code — falsifier 5 above is asserted by the same test and still passes.
`--sign-with` names a binary to run, not a capability to exercise: the secret
key is opened by minisign, the passphrase is read by minisign from the terminal,
and neither crosses this process. The relay inherits stdio precisely so that
prompt stays between the human and minisign;
`the_relay_hands_the_passphrase_prompt_straight_to_the_terminal` fails the build
if a future change captures those streams. `$COSMON_MINISIGN_BIN` redirects to
another signer (a wrapper, a smartcard); it cannot make cosmon the signer.

Verification is untouched and still happens at the **read** of the ledger.
`--by` is still inside the signed bytes — `--sign-with` does not exempt it, it
makes it impossible to get wrong, because the bytes signed are the bytes just
printed.

**Two measured traps, closed.** `--by` omitted was a refusal that named nothing;
it now says that `granted_by` came from `$USER` and that the field is inside the
signature. A rotated key produced the generic "does not authorise this
transfer"; it now names both key ids and the file the pinned one is read from.

**Additional falsifiers**, all in `takeover_unforgeable.rs`:

8. An agent that can run `--sign-with` but cannot answer the passphrase prompt
   obtains a seat (`sign_with_confers_nothing_without_the_passphrase`).
9. A signer that runs under a key the galaxy does not pin obtains a seat
   (`sign_with_confers_nothing_under_a_key_this_galaxy_does_not_trust`).
10. The relay reads the passphrase itself, rather than leaving the prompt to
    minisign on the operator's terminal.
11. `--sign-with` signs without showing the operator what is being authorised.
