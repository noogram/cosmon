# Immutability — the digests, before and after

The claim is that this molecule left every historical verdict byte-identical.
A claim like that is worth exactly the measurement behind it, so here is the
measurement: the twenty molecule-state verdicts were hashed **before any file
of this deliverable existed**, and hashed again after everything was written.

`docs/benches/issue-20-door-4-differential.md` (`V-21`) is omitted from this
table only because it is tracked by git, where `git status` already answers the
same question; it appears in `verdict-fingerprints.sha256` with the other
twenty.

## BEFORE — captured at the start of the molecule, nothing yet written

    07617f05be51de4f4afc62c7ab700e98b4b238b8ec1d75299abce2efb085d209  cmbverify-20260725-ed95/verdict.md
    1cea70905d68a6c5471595dcd9a9fae6e75b3ec3aa4cd24c233de73311491f7f  task-20260723-d1b2/verdict.json
    2bb083be738df16594205002b76c71f54330724da6b8d2fcd9bc87be45407b6c  cmbverify-20260725-ed95/verdict.json
    2edbd04c859144f917612405a982a0529c60b535a19208e93e55726fe891d916  task-20260723-631e/verdict.json
    3c2465cd4162c14b512beb479c131562ec7ae7488d85339a0389251d28c2505b  task-20260725-1e40/committee-verdict.md
    4bfc42a691a2a0828b8515965fb7d68eb13ab6eae8f0a8fc7d3593d7f491c558  cmbverify-20260725-186c/verdict.md
    77c60be765f66dbcf852c1d58eeef959d68a7620415ca2e1dddf7cff9a33bf64  cmbverify-20260725-186c/verdict.json
    7be87b4d8df0aa708013385d03e48c90029180790dce17316a09431b57ec33cb  repro-20260723-a38d/verdict.json
    859c25b97519b4a57acaa7cd5335d3fca7304f50a46f062ea94210cfab8834ab  task-20260725-3866/verdict.json
    866104eab3ce7e18ad9418449fce33116f0ff5e9d7b48f458fd1dcda3f80d007  task-20260723-b0a5/verdict.json
    9d6acbe88289cdfad2bcb5d05494e6d0da4c8ac43ffffc8f4cdfb43bee89911e  task-20260723-f18f/verdict.json
    c79c886bdd1d0ddd2c6b0717ff398c7fc09a973923fe0c49aca2929977b11dc3  task-20260725-97da/verdict.json
    cbcb5892e17d51e6a7b296bdf2a4b6f21c2211b05b41d86cd4c4876d16dbf224  task-20260723-c710/verdict.json
    cd87f2cd38faba24f1e1aa57da889e07bd207bc5fb01dc4b41c2a878174ec07a  task-20260723-5371/verdict.json
    d349805883819524b4e8ca72b939921560330259fd96b986282dd05339236969  task-20260725-9a44/contract-verdict.json
    d5f2261c0f94de44f3e6f1c5438ada8d5dd6b9a3ce4c7893ae16a0491d7b887b  task-20260723-5be4/verdict.json
    e2b5d64173fc6841e553ed50f4cb0dd416865c6a76354678d56ce1cc8ae8ebb9  bug-closure-20260725-8c79/verdict.md
    e5ddc4c4605bd8bea99070c6d7a5e7ae0333511acf817fa6de48172ad281bd51  task-20260723-d94b/verdict.json
    f41bd387212d51ddf427c1949bf016e5514227adbe60860a9c6e2b25a19129f3  converge-20260723-a767/converge-verdict.json
    f738ef601e6436ea5641edca424da4f1a541b47c4180ebc03f55746b4f451bdf  task-20260725-f02f/committee-verdict.md

## AFTER — recomputed once every artefact was in place

    07617f05be51de4f4afc62c7ab700e98b4b238b8ec1d75299abce2efb085d209  cmbverify-20260725-ed95/verdict.md
    1cea70905d68a6c5471595dcd9a9fae6e75b3ec3aa4cd24c233de73311491f7f  task-20260723-d1b2/verdict.json
    2bb083be738df16594205002b76c71f54330724da6b8d2fcd9bc87be45407b6c  cmbverify-20260725-ed95/verdict.json
    2edbd04c859144f917612405a982a0529c60b535a19208e93e55726fe891d916  task-20260723-631e/verdict.json
    3c2465cd4162c14b512beb479c131562ec7ae7488d85339a0389251d28c2505b  task-20260725-1e40/committee-verdict.md
    4bfc42a691a2a0828b8515965fb7d68eb13ab6eae8f0a8fc7d3593d7f491c558  cmbverify-20260725-186c/verdict.md
    77c60be765f66dbcf852c1d58eeef959d68a7620415ca2e1dddf7cff9a33bf64  cmbverify-20260725-186c/verdict.json
    7be87b4d8df0aa708013385d03e48c90029180790dce17316a09431b57ec33cb  repro-20260723-a38d/verdict.json
    859c25b97519b4a57acaa7cd5335d3fca7304f50a46f062ea94210cfab8834ab  task-20260725-3866/verdict.json
    866104eab3ce7e18ad9418449fce33116f0ff5e9d7b48f458fd1dcda3f80d007  task-20260723-b0a5/verdict.json
    9d6acbe88289cdfad2bcb5d05494e6d0da4c8ac43ffffc8f4cdfb43bee89911e  task-20260723-f18f/verdict.json
    c79c886bdd1d0ddd2c6b0717ff398c7fc09a973923fe0c49aca2929977b11dc3  task-20260725-97da/verdict.json
    cbcb5892e17d51e6a7b296bdf2a4b6f21c2211b05b41d86cd4c4876d16dbf224  task-20260723-c710/verdict.json
    cd87f2cd38faba24f1e1aa57da889e07bd207bc5fb01dc4b41c2a878174ec07a  task-20260723-5371/verdict.json
    d349805883819524b4e8ca72b939921560330259fd96b986282dd05339236969  task-20260725-9a44/contract-verdict.json
    d5f2261c0f94de44f3e6f1c5438ada8d5dd6b9a3ce4c7893ae16a0491d7b887b  task-20260723-5be4/verdict.json
    e2b5d64173fc6841e553ed50f4cb0dd416865c6a76354678d56ce1cc8ae8ebb9  bug-closure-20260725-8c79/verdict.md
    e5ddc4c4605bd8bea99070c6d7a5e7ae0333511acf817fa6de48172ad281bd51  task-20260723-d94b/verdict.json
    f41bd387212d51ddf427c1949bf016e5514227adbe60860a9c6e2b25a19129f3  converge-20260723-a767/converge-verdict.json
    f738ef601e6436ea5641edca424da4f1a541b47c4180ebc03f55746b4f451bdf  task-20260725-f02f/committee-verdict.md

## Difference

    $ diff before after
    (no output)

    IDENTICAL: 20/20 pre-molecule digests match the recorded fingerprints

Twenty of twenty. No verdict was opened for writing at any point. No
`superseded_by` field was added to any of them — deliberately: writing such a
field would itself have changed the bytes this table exists to protect, and it
would have been anachronistic besides. The linkage lives in
`succession-register.jsonl`, which is a new file and touches nothing.

Condition (a) is re-checkable at any time, by anyone, without this file:

    scripts/check-verdict-provenance.py
