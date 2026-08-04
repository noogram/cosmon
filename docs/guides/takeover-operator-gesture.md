# Handing over the controls — the operator gesture

Governed by [ADR-171](../adr/171-the-operator-gesture-is-a-signature-not-a-string.md)
and [ADR-168](../adr/168-a-co-pilot-inherits-the-session-substrate-not-its-delivery-contract.md).

Moving the PRIMARY seat from one pilot to another is the one thing in cosmon an
agent may never do for itself. This page is how you do it, and why it takes
three commands instead of one.

## Why it is not just `--by yourname`

It used to be. `cs sessions takeover grant --to X --by emmanuel` recorded the
string `emmanuel`, and nothing checked that a human had typed it. In the M7
dogfood the agent that ran that command was the agent it seated — which means
the rule "only an operator hands over the controls" was a habit, not a
mechanism.

The adversary is not somebody on the network. It is a process running as *you*,
on *your* machine, with *your* files. It can read anything you can read and run
anything you can run. A check on the terminal, an environment variable, or a
one-time token in a file are all things it can satisfy, because they test what
the caller looks like rather than what it knows.

There is exactly one thing it cannot do: sign with a key it does not have.

So cosmon does something slightly unusual — **it can check the seal and owns no
stamp**. There is no command in `cs` that signs a takeover. You sign, with
`minisign`, using a passphrase that lives in your head.

## One-time setup: pin your key

```sh
cd <your galaxy>
minisign -G -p .cosmon/takeover.pub -s ~/.config/cosmon/takeover.key
git add .cosmon/takeover.pub && git commit -m "chore: pin the takeover trust root"
```

Two details matter.

**Give the secret key a passphrase.** `minisign -G` asks for one; do not press
enter through it. The passphrase is the whole mechanism. A secret key sitting
unencrypted on the same disk is a secret the agent has.

**Commit the public key.** A process running as you can overwrite
`.cosmon/takeover.pub` with a key of its own — cosmon cannot stop that. What it
can do is make it *loud*: the key id is recorded in every grant, and if the
`.pub` file is tracked, swapping it is a line in a diff somebody reads.

Check what is in force at any time:

```sh
cs sessions takeover trust
```

If nothing is pinned, **no transfer is possible at all**. That is deliberate:
deleting the trust root has to stop hand-overs, not unlock them. Otherwise the
bypass would be one `rm`.

## The hand-over, in three commands

A pilot asks (this confers nothing):

```sh
cs sessions takeover request --mission task-20260731-e4d0 \
    --from codex-copilot --reason 'the primary is out of quota'
# → requested req-45f8d6d92405 …
```

You print the transfer, sign it, and present the signature:

```sh
cs sessions takeover challenge --mission task-20260731-e4d0 \
    --request req-45f8d6d92405 --by emmanuel > takeover.txt

cat takeover.txt
# cosmon-takeover-grant-v1
# mission=task-20260731-e4d0
# holder=codex-copilot
# epoch=2
# granted_by=emmanuel
# ttl=none

minisign -Sm takeover.txt          # asks for your passphrase

cs sessions takeover grant --mission task-20260731-e4d0 \
    --request req-45f8d6d92405 --by emmanuel \
    --attestation takeover.txt.minisig
```

**Read the challenge before signing it.** Those six lines are the entire
transfer: which mission, which pilot, at which generation, claimed by whom, for
how long. Nothing outside them is authorised, and changing any of them
afterwards breaks the signature.

Add `--ttl 3600` to both the challenge and the grant if you want the seat to
lapse on its own. The ttl is inside the signed bytes, so it has to match.

## Things that will happen, and what they mean

**"the attestation does not authorise this transfer".** The bytes you signed
and the bytes cosmon computed differ. The error prints the challenge cosmon
built; diff it against your `takeover.txt`. The usual cause is a `--by` that
differs between the two commands (it defaults to `$USER`), or somebody else
granting in between and moving the epoch. Re-run `challenge`, re-sign.

**"signed by key … — the pinned operator key is …".** The signature is valid
but by a different key than the one this galaxy pins. Either you signed with the
wrong secret, or the trust root is not the one you think it is — check
`cs sessions takeover trust`.

**A grant appears in `takeover show --history` marked
`NOT AN OPERATOR GESTURE`.** Somebody wrote a line into the ledger that no
operator signed. It confers nothing — every line is re-checked when it is read,
so a hand-written grant seats nobody. It is shown rather than hidden precisely
so you find out.

## What this does not protect against

Stated plainly, because a security claim that overreaches is worse than none.

The trust root is a file on the same disk as the agent. An agent running as you
can replace it with its own public key, sign a grant with the matching secret,
and seat itself. Cosmon cannot prevent that from inside a process the agent also
controls.

What changed is the cost and the visibility. Forging a hand-over now requires
either a secret you never put on that disk, or a swap of the trust root that
shows up as a changed key id in an append-only ledger and as a diff in a tracked
file. Closing the last gap needs a root of trust off the host — a hardware
token, a second machine — and is not what this mechanism is.
