# Batch-sign pilot takeovers

Use a batch when one operator must approve several independent pilot-seat
transfers. Each grant remains bound to one mission, one holder session and one
next epoch; batching only reduces the number of passphrase prompts.

For every intended transfer, obtain the versioned JSON challenge and show its
structured `challenge` fields to the operator. Write the `bytes` string exactly
as returned, including its final newline, to a separate file:

```sh
cs --json sessions takeover challenge \
  --mission task-a --to session-a --by operator > challenge-a.json
jq -rj .bytes challenge-a.json > challenge-a.txt

cs --json sessions takeover challenge \
  --mission task-b --to session-b --by operator > challenge-b.json
jq -rj .bytes challenge-b.json > challenge-b.txt
```

After the operator has checked the complete list, the operator runs one signer
invocation. An agent or unattended client must never produce this signature:

```sh
minisign -S -s /path/to/operator.key -m challenge-a.txt challenge-b.txt
```

Submit each detached signature with the same arguments used to obtain its
challenge:

```sh
cs sessions takeover grant \
  --mission task-a --to session-a --by operator \
  --attestation challenge-a.txt.minisig

cs sessions takeover grant \
  --mission task-b --to session-b --by operator \
  --attestation challenge-b.txt.minisig
```

Do not reuse a signature after any mission, holder session, operator label,
time-to-live or epoch changes. Generate and show a fresh challenge instead.
