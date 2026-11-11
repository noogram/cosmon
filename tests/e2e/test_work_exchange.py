# SPDX-License-Identifier: AGPL-3.0-only
"""Issue #147 W4 acceptance: a cross-machine ``work`` exchange, end to end.

The in-process route tests (``work_routes_test.rs``) prove the handlers.
This file proves the thing W4 is *for*: two clients with **distinct OIDC
identities** — one per machine — exchange advisory work through the real
compose stack, using the real ``cosmon-remote`` binary, and every verdict is
judged on the **host store read from inside the service container**
(envelopes, payload files, receipts), never on the HTTP status alone. That is
the method of the #128 harvest matrix (``test_harvest_matrix.py``).

Setup, all by documented operator gestures on the service host:

* the compose stack (RPP adapter + mock OIDC), freshly initialised;
* one declared work (``cs work declare`` inside the service container) with
  five seats — ``pilot`` (the owner's machine), ``remote`` (the other
  machine), ``reader`` and ``temp`` (identities used for refusals) and
  ``sink`` (a seat nobody listens on);
* one collaboration binding per identity (``cs collaboration bind`` inside the
  service container), its proof handed to the client over stdin;
* the **owner** client runs on the host (a real ``login`` with the
  authorization-code + PKCE flow); the **collaborator** client runs in its own
  container on the compose network with **no bind mount and no published
  port** (``harness.work.ContainerCli``), authenticating as another subject.

Nothing here edits production code or adds a route: a witness that needs
something the documented commands cannot reach is recorded as a finding in the
test docstring and the test asserts what is actually true.
"""
from __future__ import annotations

import json
import os
import subprocess
import time
import uuid
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple

import pytest

from harness.work import ContainerCli, build_client_image, mint_token, raw_request
from harness.remote import RemoteCli

pytestmark = pytest.mark.stack

READ = "cosmon:work:read"
WRITE = "cosmon:work:write"
SUB_COLLABORATOR = "e2e-collaborator"
SUB_READER = "e2e-reader"
SUB_TEMP = "e2e-temp"

#: Small budgets so the bounded behaviours are reachable in seconds. They are
#: the documented ``cs work declare`` flags, not a test hook.
BUDGET = {
    "max_payload_bytes": 1024,
    "max_messages_per_seat": 5,
    "redeliver_after_secs": 4,
    "max_delivery_attempts": 3,
}


# -- the host store, read from inside the service container -----------------


class HostStore:
    """The owner's work store as the host holds it.

    Every read is a command run **inside the service container**, so the
    witness is the bytes the service itself would read, not a host-side view
    of the bind mount.
    """

    def __init__(self, stack, cfg, owner: str) -> None:
        self.stack, self.cfg, self.owner = stack, cfg, owner
        self.state = f"/cosmon/galaxies/{cfg.noyau}/.cosmon/state"
        self.dir = self.molecule_dir(owner)

    def molecule_dir(self, molecule: str) -> str:
        found = self.stack.service_exec(
            "sh", "-c", f"find {self.state} -maxdepth 5 -type d -name {molecule}",
        ).stdout.split()
        assert len(found) == 1, f"expected one directory for {molecule}, found {found}"
        return found[0]

    def sh(self, script: str, check: bool = True) -> str:
        return self.stack.service_exec("sh", "-c", script, check=check).stdout

    def work_files(self) -> Dict[str, str]:
        """Every file under ``work/`` → its sha256. Equal maps mean nothing changed."""
        out = self.sh(f"cd {self.dir} && find work -type f | sort | xargs -r sha256sum")
        return {line.split()[1]: line.split()[0] for line in out.splitlines() if line.strip()}

    def envelope(self, key: str) -> Optional[dict]:
        out = self.sh(f"cat {self.dir}/work/envelopes/{key}.json 2>/dev/null", check=False)
        return json.loads(out) if out.strip() else None

    def envelopes(self) -> List[str]:
        out = self.sh(f"ls {self.dir}/work/envelopes 2>/dev/null", check=False)
        return sorted(out.split())

    def receipts(self, key: str) -> List[dict]:
        out = self.sh(f"cat {self.dir}/work/receipts/{key}.jsonl 2>/dev/null", check=False)
        return [json.loads(line) for line in out.splitlines() if line.strip()]

    def receipt_bytes(self, key: str) -> str:
        return self.sh(f"cat {self.dir}/work/receipts/{key}.jsonl 2>/dev/null", check=False)

    def payload(self, envelope: dict) -> str:
        # `payload_ref` is relative to the owner's molecule directory.
        return self.sh(f"cat {self.dir}/{envelope['payload_ref']}")

    def stages(self, key: str) -> List[str]:
        return [r["stage"]["stage"] for r in self.receipts(key)]

    def lifecycle(self, molecules: List[str]) -> Dict[str, Any]:
        """Everything that is NOT a work record, for the molecules involved.

        §5 says no message confers lifecycle authority. The witness is the
        molecule directories (state, events, every file outside ``work/``),
        the set of molecules in the tenant, the tenant repository's branches
        and worktrees, and any tmux server a dispatch would have opened.
        """
        snap: Dict[str, Any] = {}
        for molecule in molecules:
            directory = self.molecule_dir(molecule)
            out = self.sh(
                f"cd {directory} && find . -type f ! -path './work/*' ! -name work-ref.json "
                "| sort | xargs -r sha256sum"
            )
            snap[molecule] = {l.split()[1]: l.split()[0] for l in out.splitlines() if l.strip()}
        parent = os.path.dirname(self.dir)
        snap["molecules"] = sorted(self.sh(f"ls {parent}").split())
        galaxy = f"/cosmon/galaxies/{self.cfg.noyau}"
        snap["branches"] = self.sh(f"git -C {galaxy} branch --list --format='%(refname:short)'").split()
        snap["worktrees"] = self.sh(f"git -C {galaxy} worktree list --porcelain").count("worktree ")
        snap["tmux"] = self.sh("tmux ls 2>&1; true").strip()
        return snap


@dataclass
class Party:
    """One admitted identity and the client that speaks for it."""

    seat: str
    sub: str
    cli: Any
    attachment: str
    binding_id: str
    proof: str = field(repr=False)
    token: str = field(repr=False, default="")


@dataclass
class World:
    owner: str
    molecules: Dict[str, str]
    parties: Dict[str, Party]
    store: HostStore
    revision: str
    lifecycle_before: Dict[str, Any]
    collaborator_isolation: Dict[str, Any]
    client_image_id: str
    #: Outcomes the witnesses share, so each test can run alone (`-k`).
    cache: Dict[str, Any] = field(default_factory=dict)


# -- fixtures -----------------------------------------------------------------


@pytest.fixture(scope="session")
def client_image(cfg, recorder, compose_stack) -> str:
    """The Linux ``cosmon-remote`` image the collaborator container runs."""
    tag = os.environ.get("RPP_E2E_CLIENT_IMAGE", "cs-rpp-e2e-client:e2e")
    if os.environ.get("RPP_E2E_CLIENT_IMAGE_PREBUILT") == "1":
        inspect = subprocess.run(["docker", "image", "inspect", tag], capture_output=True, text=True)
        assert inspect.returncode == 0, f"pinned client image {tag} is missing"
    else:
        build_client_image(cfg, recorder, tag)
    return tag


def _json_line(text: str) -> dict:
    return json.loads(text.strip().splitlines()[-1])


def _bind(stack, cfg, seat: str, sub: str, owner: str, scopes: Tuple[str, ...]) -> Tuple[dict, str]:
    """``cs collaboration bind`` on the host; returns (binding, proof)."""
    path = f"/tmp/proof-{seat}-{uuid.uuid4().hex[:8]}"
    args = [
        "collaboration", "bind", "--issuer", cfg.issuer, "--subject", sub,
        "--audience", cfg.audience, "--tenant", cfg.noyau,
        "--work-owner", owner, "--seat", seat, "--proof-out", path,
        "--label", f"{seat}-machine",
    ]
    for scope in scopes:
        args += ["--scope", scope]
    binding = _json_line(stack.operator_cs(*args).stdout)
    # Read once and removed: the proof never stays on the host, and is not
    # recorded in the run's artefacts.
    proof = stack.service_exec("cat", path, record_response=False).stdout.strip()
    stack.service_exec("rm", "-f", path)
    assert binding["active"] is True and binding["attachment"], binding
    return binding, proof


@pytest.fixture(scope="class")
def world(stack, cfg, logged_in, remote_binary, recorder, client_image, request) -> World:
    # -- identities, admitted to the SAME tenant (contract O2) ---------------
    owner_scopes = ("cosmon:molecule:read", "cosmon:molecule:write", "cosmon:worker:spawn", READ, WRITE)
    stack.set_primary_scopes(owner_scopes)
    stack.add_identity("collab", SUB_COLLABORATOR, (READ, WRITE))
    # `reader`'s token can read the work and nothing more: the identity
    # binding grants no write scope, while its collaboration binding below
    # does, so a refusal can only be the token's missing scope.
    stack.add_identity("reader", SUB_READER, (READ,))
    stack.add_identity("temp", SUB_TEMP, (READ, WRITE))
    stack.reload_bindings()

    # -- the work: five molecules, one declared roster -----------------------
    names = ["pilot", "remote", "reader", "temp", "sink"]
    molecules: Dict[str, str] = {}
    for name in names:
        rc, payload, stderr = logged_in.nucleate("task-work", f"work seat {name}")
        assert rc == 0, f"nucleate {name}: {stderr[-400:]}"
        molecules[name] = payload["molecule"]["id"]
    owner = molecules["pilot"]
    declare = ["work", "declare", owner]
    for name in names:
        declare += ["--seat", f"{name}={molecules[name]}"]
    for flag, value in BUDGET.items():
        declare += ["--" + flag.replace("_", "-"), str(value)]
    stack.operator_cs(*declare)

    # -- the second machine: its own container, no bind mount ----------------
    collab_cli = ContainerCli(
        cfg, recorder, name=f"cs-e2e-collab-{os.getpid()}", network=stack.network,
        image=client_image, sub=SUB_COLLABORATOR,
    )
    collab_cli.start()
    request.addfinalizer(collab_cli.stop)
    collab_cli.configure()
    collab_cli.token = mint_token(cfg, SUB_COLLABORATOR)
    isolation = dict(collab_cli.isolation, **collab_cli.inspect_filesystem())

    reader_cli = RemoteCli(cfg, remote_binary, recorder, slot=f"reader-gen{stack.generation}", sub=SUB_READER)
    temp_cli = RemoteCli(cfg, remote_binary, recorder, slot=f"temp-gen{stack.generation}", sub=SUB_TEMP)
    reader_token, temp_token = mint_token(cfg, SUB_READER), mint_token(cfg, SUB_TEMP)
    for cli, token in ((reader_cli, reader_token), (temp_cli, temp_token)):
        cli.configure()
        cli.run("config", "set", "issuer", cfg.issuer, step="config issuer")
        cli.run("config", "set", "client-id", cfg.audience, step="config client-id")
        cli.token = token  # type: ignore[attr-defined]

    plan = [
        ("pilot", cfg.idp_sub, logged_in, None),
        ("remote", SUB_COLLABORATOR, collab_cli, collab_cli.token),
        ("reader", SUB_READER, reader_cli, reader_token),
        ("temp", SUB_TEMP, temp_cli, temp_token),
    ]
    parties: Dict[str, Party] = {}
    for seat, sub, cli, token in plan:
        binding, proof = _bind(stack, cfg, seat, sub, owner, (READ, WRITE))
        cli.run("config", "set", "attachment", binding["attachment"], step=f"config attachment {seat}",
                token_override="")
        rc, stored, stderr = cli.json("work", "store-proof", stdin=proof + "\n", token_override="",
                                      step=f"store-proof {seat}")
        assert rc == 0 and stored == {"stored": True}, f"store-proof {seat}: {stderr[-300:]}"
        parties[seat] = Party(
            seat=seat, sub=sub, cli=cli, attachment=binding["attachment"],
            binding_id=binding["id"], proof=proof,
            token=token or mint_token(cfg, sub),
        )
    # The non-interactive clients authenticate with the bearer minted for
    # their own subject (the interactive login's callback port is a single
    # host resource, and the container cannot dial the IdP's published port).
    for name in ("reader", "temp"):
        parties[name].cli.token = parties[name].token  # type: ignore[attr-defined]

    store = HostStore(stack, cfg, owner)
    rc, roster, stderr = logged_in.json("work", "list", owner, step="work list (revision)")
    assert rc == 0, stderr[-400:]
    image_id = subprocess.run(
        ["docker", "image", "inspect", client_image, "--format", "{{.Id}}"], capture_output=True, text=True,
    ).stdout.strip()
    return World(
        owner=owner, molecules=molecules, parties=parties, store=store,
        revision=roster["scope_revision"],
        lifecycle_before=store.lifecycle(list(molecules.values())),
        collaborator_isolation=isolation, client_image_id=image_id,
    )


# -- helpers --------------------------------------------------------------------


def _call(party: Party, *args: str, step: str) -> Tuple[int, Any, str]:
    """One CLI call as ``party``. The reader/temp slots send their bearer by env."""
    if isinstance(party.cli, RemoteCli):
        return party.cli.json(*args, step=step, bearer_flag=getattr(party.cli, "token", None))
    return party.cli.json(*args, step=step)


def _send(world: World, party: Party, to: str, key: str, text: str, *, step: str, **extra: str):
    args = ["work", "send", world.owner, "--scope-revision", world.revision, "--to", to,
            "--key", key, "--text", text]
    for flag, value in extra.items():
        args += ["--" + flag.replace("_", "-"), value]
    return _call(party, *args, step=step)


def _inbox(world: World, party: Party, *, peek: bool, step: str):
    args = ["work", "inbox", world.owner, "--scope-revision", world.revision]
    if peek:
        args.append("--peek")
    return _call(party, *args, step=step)


def _ack(world: World, party: Party, key: str, digest: str, *, step: str, note: Optional[str] = None):
    args = ["work", "ack", world.owner, key, "--scope-revision", world.revision,
            "--payload-digest", digest, "--considered"]
    if note is not None:
        args += ["--note", note]
    return _call(party, *args, step=step)


def _raw(cfg, recorder, world: World, party: Party, method: str, path: str, body=None, *,
         token: Optional[str] = None, attachment: Optional[str] = None,
         proof: Optional[str] = None, step: str):
    return raw_request(
        cfg, recorder, method, path, token=token or party.token,
        attachment=attachment or party.attachment, proof=proof or party.proof, body=body, step=step,
    )


def _send_body(world: World, to: str, key: str, text: str) -> dict:
    return {
        "scope_revision": world.revision, "recipient": to, "idempotency_key": key,
        "sender_time": "2026-11-02T12:00:00Z", "confidentiality": "internal", "payload": text,
    }


def _refused(rc: int, payload: Any, code: str, expect, why: str) -> None:
    expect.truthy(rc != 0, f"{why}: the client must exit non-zero on a refusal (rc={rc})")
    expect.equals((payload or {}).get("error"), code, why)


# -- witness 1: the round trip --------------------------------------------------

RT_KEY = "rt-1"
RT_TEXT = "alpha.txt:3 says 15 units; beta.txt:2 says 14. Sent from the other machine."


def run_round_trip(world: World, expect) -> Dict[str, Any]:
    """collaborator send -> owner peek -> owner pull -> owner ack, judged on the store.

    Run once per set and cached, so that every witness that needs a consumed
    message can run alone. All assertions live here; a failure is attributed
    to the first test that asked.
    """
    if "round_trip" in world.cache:
        return world.cache["round_trip"]
    pilot, remote, store = world.parties["pilot"], world.parties["remote"], world.store

    # roster, from both machines
    rc, from_pilot, err = _call(pilot, "work", "list", world.owner, step="W1 list (owner)")
    expect.equals(rc, 0, f"owner lists the roster: {err[-300:]}")
    rc, from_remote, err = _call(remote, "work", "list", world.owner, step="W1 list (collaborator)")
    expect.equals(rc, 0, f"collaborator lists the roster: {err[-300:]}")
    for field_ in ("owner", "scope_revision", "seats", "budget"):
        expect.equals(from_remote[field_], from_pilot[field_],
                      f"W-LIST is the same roster for both machines ({field_})")
    expect.equals(sorted(from_pilot["seats"]), sorted(world.molecules),
                  "the roster is the declared one (cs work declare)")
    host_scope = json.loads(store.sh(f"cat {store.dir}/work/scope.json"))
    expect.equals(sorted(host_scope["seats"]), sorted(from_pilot["seats"]),
                  "the HTTP roster equals scope.json read inside the service container")
    expect.equals(host_scope["budget"], from_pilot["budget"], "and so does the budget")
    expect.equals(from_pilot["budget"]["redeliver_after_secs"], BUDGET["redeliver_after_secs"],
                  "the staged small budget reached the wire")

    files_before = store.work_files()

    # send: collaborator -> pilot
    rc, sent, err = _send(world, remote, "pilot", RT_KEY, RT_TEXT, step="W1 send")
    expect.equals(rc, 0, f"collaborator sends: {err[-400:]}")
    envelope = store.envelope(RT_KEY)
    expect.truthy(envelope is not None, "the envelope is on the host store after the send")
    expect.equals(envelope["sender"], "remote",
                  "the sender is derived from the collaborator's binding, never from a request field")
    expect.equals(envelope["recipient"], "pilot", "the recipient is the owner's seat")
    expect.equals(envelope["sender_evidence"], "admitted_non_local",
                  "the store records that a non-local admission boundary vouched for the sender")
    expect.equals(sent["envelope"]["payload_digest"], envelope["payload_digest"],
                  "the digest in the reply is the digest on disk")
    expect.equals(store.payload(envelope), RT_TEXT, "the payload file holds exactly the bytes sent")
    expect.equals(store.stages(RT_KEY), ["admitted"], "admission is the only observation after a send")
    expect.equals(sent["duplicate"], False, "a first send is an admission, not a replay")
    expect.truthy(len(store.work_files()) > len(files_before), "the send added records to the host store")

    # peek: no delivery observation
    receipts_before_peek = store.receipt_bytes(RT_KEY)
    rc, peeked, err = _inbox(world, pilot, peek=True, step="W1 peek (owner)")
    expect.equals(rc, 0, f"owner peeks: {err[-300:]}")
    expect.equals([m["envelope"]["key"] for m in peeked["messages"]], [RT_KEY],
                  "the peek shows the message")
    expect.equals(peeked["messages"][0]["payload"], RT_TEXT, "with its payload")
    expect.equals(store.receipt_bytes(RT_KEY), receipts_before_peek,
                  "the peek wrote no delivery observation: receipts are byte-identical")
    expect.equals(store.stages(RT_KEY), ["admitted"], "still only the admission")

    # pull: exactly one observation
    rc, pulled, err = _inbox(world, pilot, peek=False, step="W1 pull (owner)")
    expect.equals(rc, 0, f"owner pulls: {err[-300:]}")
    expect.equals([m["envelope"]["key"] for m in pulled["messages"]], [RT_KEY], "the pull delivers the message")
    expect.equals(store.stages(RT_KEY), ["admitted", "delivery_attempted"],
                  "the pull wrote exactly one delivery observation")
    attempt = store.receipts(RT_KEY)[1]["stage"]
    expect.equals(attempt["outcome"], {"result": "submitted"},
                  "and it says 'submitted' — the stage the server observed, nothing about a model")

    # ack
    digest = envelope["payload_digest"]
    rc, acked, err = _ack(world, pilot, RT_KEY, digest, step="W1 ack (owner)")
    expect.equals(rc, 0, f"owner acknowledges: {err[-300:]}")
    expect.equals(acked["duplicate"], False, "the first acknowledgment is recorded")
    expect.equals(store.stages(RT_KEY), ["admitted", "delivery_attempted", "consumed"],
                  "the acknowledgment is one durable consumption record")
    consumed = store.receipts(RT_KEY)[2]["stage"]
    expect.equals(consumed["disposition"], "considered", "with the stated disposition")

    # the sender sees it, and the roster is unchanged
    rc, back, err = _inbox(world, remote, peek=True, step="W1 peek (collaborator)")
    expect.equals(rc, 0, f"collaborator reads its own side: {err[-300:]}")
    expect.equals([r["key"] for r in back["receipts"]], [RT_KEY],
                  "the sender is shown the consumption report for its message")
    expect.equals(back["messages"], [], "and has nothing addressed to it")
    rc, again, err = _call(remote, "work", "list", world.owner, step="W1 list (collaborator, after)")
    expect.equals(again["scope_revision"], from_pilot["scope_revision"], "the roster revision did not move")
    result = {"envelope": envelope, "digest": digest, "sent": sent}
    world.cache["round_trip"] = result
    return result


class TestWorkExchange:
    """One test set: a fresh stack, one declared work, four bound identities.

    Witness 0 is the setup's own claim (the collaborator is a second machine);
    witnesses 1-5 are the issue's. Every test can run alone: shared outcomes
    come from :func:`run_round_trip`, cached per set.
    """

    def test_00_the_collaborator_is_isolated_and_distinct(self, world: World, cfg, expect):
        """The second client has its own container, filesystem and identity.

        Isolation is read from the daemon (``docker inspect``) and from inside
        the container, not assumed from the run command: an empty mount list,
        no published port, none of the host's trees visible, a non-root user.
        The identity claim is read from the SERVER: ``auth me`` reports the
        collaborator's ``sub``, different from the owner's.
        """
        iso = world.collaborator_isolation
        expect.equals(iso["mounts"], [], "the collaborator container has no bind mount and no volume")
        expect.equals(iso["published_ports"], {}, "the collaborator container publishes no port")
        expect.equals(iso["host_galaxy_visible"], "",
                      "neither the host's home nor the service's /cosmon tree exists inside the collaborator")
        expect.truthy(iso["uid"] not in ("", "0"), "the client runs as a non-root user")
        remote, pilot = world.parties["remote"], world.parties["pilot"]
        rc, me_remote, stderr = remote.cli.json("auth", "me", step="auth me (remote)")
        expect.equals(rc, 0, f"the collaborator's bearer is admitted: {stderr[-300:]}")
        rc, me_pilot, stderr = pilot.cli.json("auth", "me", step="auth me (pilot)")
        expect.equals(rc, 0, f"the owner's login credential is admitted: {stderr[-300:]}")
        expect.equals(me_remote["sub"], SUB_COLLABORATOR, "the server resolved the collaborator's own subject")
        expect.equals(me_pilot["sub"], cfg.idp_sub, "the server resolved the owner's subject")
        expect.truthy(me_remote["sub"] != me_pilot["sub"], "the two clients are distinct OIDC identities")
        expect.truthy(pilot.attachment != remote.attachment, "each machine has its own attachment")
        expect.truthy(pilot.proof != remote.proof, "each attachment has its own proof")

    def test_01_round_trip(self, world: World, expect):
        """W1: collaborator send -> owner peek -> owner pull -> owner ack.

        Judged on the host store read inside the service container: the
        envelope and its payload file, and the receipts file whose lines are
        the delivery observations. The peek leaves the receipts byte-identical;
        the pull adds exactly one ``delivery_attempted``; the ack adds one
        ``consumed``. ``work list`` is the same roster from both machines.
        """
        run_round_trip(world, expect)

    def test_02_idempotence(self, world: World, cfg, recorder, expect):
        """W2: one key + same content = one admission; changed content = key_collision;
        a repeated ack changes nothing.
        """
        trip = run_round_trip(world, expect)
        remote, pilot, store = world.parties["remote"], world.parties["pilot"], world.store

        key = "idem-1"
        rc, first, err = _send(world, remote, "sink", key, "same bytes", step="W2 send 1")
        expect.equals(rc, 0, f"first send: {err[-300:]}")
        expect.equals(first["duplicate"], False, "the first send is an admission")
        envelopes_after_first = store.envelopes()
        receipts_after_first = store.receipt_bytes(key)
        rc, second, err = _send(world, remote, "sink", key, "same bytes", step="W2 send 2 (replay)")
        expect.equals(rc, 0, f"replay: {err[-300:]}")
        expect.equals(second["duplicate"], True, "the replay is reported as a duplicate")
        expect.equals(second["envelope"]["payload_digest"], first["envelope"]["payload_digest"],
                      "and returns the same admitted envelope")
        expect.equals(store.envelopes(), envelopes_after_first, "no second envelope on the host")
        expect.equals(store.receipt_bytes(key), receipts_after_first,
                      "no second admission record: receipts are byte-identical")
        expect.equals(store.stages(key), ["admitted"], "exactly one logical admission")

        # same key, different content
        before = store.work_files()
        rc, collided, err = _send(world, remote, "sink", key, "DIFFERENT bytes", step="W2 send 3 (collision)")
        _refused(rc, collided, "key_collision", expect, "same key + different content")
        status, body = _raw(cfg, recorder, world, remote, "POST", f"/v1/work/{world.owner}/messages",
                            _send_body(world, "sink", key, "DIFFERENT bytes"), step="W2 collision (status)")
        expect.equals((status, (body or {}).get("error")), (409, "key_collision"),
                      "§6: key_collision is HTTP 409")
        expect.equals(store.work_files(), before, "the colliding send left nothing behind")
        expect.equals(store.payload(store.envelope(key)), "same bytes", "the first content is intact")

        # repeated ack
        receipts_before = store.receipt_bytes(RT_KEY)
        rc, dup, err = _ack(world, pilot, RT_KEY, trip["digest"], step="W2 ack (repeat)")
        expect.equals(rc, 0, f"a repeated ack is not an error: {err[-300:]}")
        expect.equals(dup["duplicate"], True, "the repeated ack is reported as a duplicate")
        expect.equals(store.receipt_bytes(RT_KEY), receipts_before,
                      "and wrote nothing: the receipts file is byte-identical")
        expect.equals(store.stages(RT_KEY).count("consumed"), 1, "one consumption record, not two")

    def test_03_bounded_redelivery(self, world: World, expect):
        """W3: an unacknowledged message is re-offered after ``redeliver_after_secs``,
        at most ``max_delivery_attempts`` times.

        Once attempts are used up, work list shows ``exhausted`` for the sender
        and recipient. Pull remains a successful empty response, and a later
        acknowledgment records consumption without reopening delivery.
        """
        run_round_trip(world, expect)
        remote, pilot, store = world.parties["remote"], world.parties["pilot"], world.store
        wait = BUDGET["redeliver_after_secs"]
        key = "redeliver-1"
        rc, sent, err = _send(world, remote, "pilot", key, "please look at the diff", step="W3 send")
        expect.equals(rc, 0, f"send: {err[-300:]}")
        digest = sent["envelope"]["payload_digest"]

        def pull(step: str) -> Tuple[List[str], float]:
            rc, got, err = _inbox(world, pilot, peek=False, step=step)
            expect.equals(rc, 0, f"{step}: {err[-300:]}")
            return [m["envelope"]["key"] for m in got["messages"]], time.time()

        offered, t1 = pull("W3 pull 1")
        expect.equals(offered, [key], "first pull: offered")
        offered, t2 = pull("W3 pull 2 (too early)")
        expect.truthy(t2 - t1 < wait, f"precondition: the second pull ran {t2 - t1:.1f}s after the first (< {wait}s)")
        expect.equals(offered, [], f"a pull inside redeliver_after_secs={wait} is not offered the message")
        expect.equals(store.stages(key).count("delivery_attempted"), 1, "and costs no attempt")

        time.sleep(max(0.0, wait + 0.5 - (time.time() - t1)))
        offered, t3 = pull("W3 pull 3 (after the wait)")
        expect.equals(offered, [key], "after redeliver_after_secs the message is offered again")
        time.sleep(wait + 0.5)
        offered, _ = pull("W3 pull 4")
        expect.equals(offered, [key], "third and last permitted attempt")
        expect.equals(store.stages(key).count("delivery_attempted"), BUDGET["max_delivery_attempts"],
                      "the host store holds exactly max_delivery_attempts observations")
        time.sleep(wait + 0.5)
        before = store.receipt_bytes(key)
        offered, _ = pull("W3 pull 5 (budget used)")
        expect.equals(offered, [], "attempts exhausted: the message is no longer offered")
        expect.equals(store.receipt_bytes(key), before, "and no further observation is written")
        expect.equals(store.stages(key).count("delivery_attempted"), BUDGET["max_delivery_attempts"],
                      "never more than max_delivery_attempts, however often the owner pulls")
        for party in (remote, pilot):
            rc, listed, err = _call(party, "work", "list", world.owner, step="W3 list exhausted")
            expect.equals(rc, 0, f"list exhausted: {err[-300:]}")
            states = {message["key"]: message["delivery_state"] for message in listed["messages"]}
            expect.equals(states.get(key), "exhausted", "work list exposes delivery exhaustion")
        expect.equals(store.receipt_bytes(key), before, "listing writes no observation")
        # A consumption report preserves the independently derived exhaustion.
        rc, acked, err = _ack(world, pilot, key, digest, step="W3 ack after exhaustion")
        expect.equals(rc, 0, f"an exhausted message can still be acknowledged: {err[-300:]}")
        expect.equals(store.stages(key)[-1], "consumed", "and the host records it")
        rc, listed, err = _call(remote, "work", "list", world.owner, step="W3 list after ack")
        expect.equals(rc, 0, f"list after ack: {err[-300:]}")
        states = {message["key"]: message["delivery_state"] for message in listed["messages"]}
        expect.equals(states.get(key), "exhausted", "ack does not reset delivery exhaustion")
        offered, _ = pull("W3 pull after ack")
        expect.equals(offered, [], "ack never reopens delivery")

    def test_04a_scope_missing_is_checked_before_anything_is_read(self, world: World, cfg, recorder, expect):
        """W4: a token without ``cosmon:work:write`` is refused ``scope_missing`` (403),
        leaving no envelope, payload or receipt, and before any resource read.

        The proof that it comes before any read: the same refusal is returned
        for an owner that does not exist (where any read would have produced
        ``unknown_work`` or ``binding_missing``). The reader's token CAN read
        (``list`` succeeds), and its collaboration binding grants write, so the
        scope is the only thing missing.
        """
        run_round_trip(world, expect)
        reader, store = world.parties["reader"], world.store
        rc, listed, err = _call(reader, "work", "list", world.owner, step="W4 reader list")
        expect.equals(rc, 0, f"the reader's token carries work:read: {err[-300:]}")
        before = store.work_files()
        rc, payload, err = _send(world, reader, "pilot", "reader-attempt", "I may not write", step="W4 reader send")
        _refused(rc, payload, "scope_missing", expect, "a token without cosmon:work:write")
        status, body = _raw(cfg, recorder, world, reader, "POST", f"/v1/work/{world.owner}/messages",
                            _send_body(world, "pilot", "reader-attempt", "I may not write"),
                            step="W4 reader send (status)")
        expect.equals((status, (body or {}).get("error")), (403, "scope_missing"), "§6: scope_missing is HTTP 403")
        expect.equals(store.envelope("reader-attempt"), None, "no envelope")
        expect.equals(store.receipts("reader-attempt"), [], "no receipt")
        expect.equals(store.work_files(), before, "nothing at all was written to the work store")
        # ack needs the same scope
        rc, payload, err = _ack(world, reader, RT_KEY, world.cache["round_trip"]["digest"], step="W4 reader ack")
        _refused(rc, payload, "scope_missing", expect, "an acknowledgment also needs cosmon:work:write")
        # before any resource read
        ghost = "task-20990101-dead"
        status, body = _raw(cfg, recorder, world, reader, "POST", f"/v1/work/{ghost}/messages",
                            _send_body(world, "pilot", "reader-attempt", "x"), step="W4 reader send (no such owner)")
        expect.equals((status, (body or {}).get("error")), (403, "scope_missing"),
                      "for an owner that does not exist the answer is still scope_missing: the scope is "
                      "checked before the work, the binding or the store is read")
        expect.equals(store.work_files(), before, "still nothing written")

    def test_04b_another_identitys_seat_is_refused(self, world: World, cfg, recorder, expect):
        """W4: an identity acting for a seat that is not its own is refused.

        Two forms. (1) The collaborator presents the OWNER's attachment and
        proof with its own token, and the owner's token carries the
        collaborator's attachment: ``binding_missing`` (403) both ways; the
        proof is not a bearer credential on its own. (2) A message addressed to
        a name that is not a seat is refused ``not_a_member`` (403). Each leaves
        the work store byte-identical.
        """
        run_round_trip(world, expect)
        remote, pilot, store = world.parties["remote"], world.parties["pilot"], world.store
        before = store.work_files()
        body = _send_body(world, "sink", "impersonation", "as the owner")
        status, got = _raw(cfg, recorder, world, remote, "POST", f"/v1/work/{world.owner}/messages", body,
                           attachment=pilot.attachment, proof=pilot.proof, step="W4 collaborator uses owner's proof")
        expect.equals((status, (got or {}).get("error")), (403, "binding_missing"),
                      "the collaborator's token with the owner's attachment and proof is refused")
        status, got = _raw(cfg, recorder, world, pilot, "POST", f"/v1/work/{world.owner}/messages", body,
                           attachment=remote.attachment, proof=remote.proof, step="W4 owner uses collaborator's proof")
        expect.equals((status, (got or {}).get("error")), (403, "binding_missing"),
                      "and the owner's token with the collaborator's attachment and proof")
        status, got = _raw(cfg, recorder, world, remote, "POST", f"/v1/work/{world.owner}/messages", body,
                           attachment=remote.attachment, proof=pilot.proof, step="W4 wrong proof")
        expect.equals((status, (got or {}).get("error")), (403, "binding_missing"),
                      "the right attachment with another attachment's proof")
        expect.equals(store.envelope("impersonation"), None, "no impersonated envelope exists on the host")
        expect.equals(store.work_files(), before, "the work store is byte-identical")

        rc, payload, err = _send(world, remote, "ghost", "to-nobody", "no such seat", step="W4 send to a non-seat")
        _refused(rc, payload, "not_a_member", expect, "a recipient that is not a seat of the work")
        status, got = _raw(cfg, recorder, world, remote, "POST", f"/v1/work/{world.owner}/messages",
                           _send_body(world, "ghost", "to-nobody", "no such seat"), step="W4 non-seat (status)")
        expect.equals((status, (got or {}).get("error")), (403, "not_a_member"), "§6: not_a_member is HTTP 403")
        expect.equals(store.work_files(), before, "still byte-identical")

    def test_04c_payload_too_large(self, world: World, cfg, recorder, expect):
        """W4: a payload over ``budget.max_payload_bytes`` is ``payload_too_large`` (413);
        an oversized acknowledgment note is the same code. Neither leaves a record.
        """
        trip = run_round_trip(world, expect)
        remote, pilot, store = world.parties["remote"], world.parties["pilot"], world.store
        before = store.work_files()
        big = "x" * (BUDGET["max_payload_bytes"] + 1)
        rc, payload, err = _send(world, remote, "pilot", "too-big", big, step="W4 oversized payload")
        _refused(rc, payload, "payload_too_large", expect, "payload one byte over the declared limit")
        status, got = _raw(cfg, recorder, world, remote, "POST", f"/v1/work/{world.owner}/messages",
                           _send_body(world, "pilot", "too-big", big), step="W4 oversized payload (status)")
        expect.equals((status, (got or {}).get("error")), (413, "payload_too_large"), "§6: HTTP 413")
        expect.equals(store.envelope("too-big"), None, "no envelope")
        expect.equals(store.receipts("too-big"), [], "no receipt")
        # exactly at the limit is accepted (the bound is inclusive)
        rc, ok, err = _send(world, remote, "sink", "at-limit", "y" * BUDGET["max_payload_bytes"], step="W4 payload at the limit")
        expect.equals(rc, 0, f"a payload of exactly max_payload_bytes is admitted: {err[-300:]}")
        before = store.work_files()
        rc, payload, err = _ack(world, pilot, RT_KEY, trip["digest"], step="W4 oversized note",
                                note="n" * 4097)
        _refused(rc, payload, "payload_too_large", expect, "an acknowledgment note over 4096 bytes")
        expect.equals(store.work_files(), before, "the oversized note left nothing behind")

    def test_04d_message_budget_then_revocation(self, world: World, cfg, recorder, expect):
        """W4: the seat's message allowance gives ``budget_exhausted`` (429); a revoked
        binding gives ``binding_revoked`` (403). Neither leaves a record, and the
        history written before the revocation stays.
        """
        run_round_trip(world, expect)
        temp, store = world.parties["temp"], world.store
        limit = BUDGET["max_messages_per_seat"]
        for i in range(limit):
            rc, _, err = _send(world, temp, "sink", f"temp-{i}", f"message {i}", step=f"W4 temp send {i}")
            expect.equals(rc, 0, f"message {i + 1} of {limit} is within the allowance: {err[-300:]}")
        before = store.work_files()
        rc, payload, err = _send(world, temp, "sink", "temp-over", "one too many", step="W4 temp over budget")
        _refused(rc, payload, "budget_exhausted", expect, "the seat used its message allowance")
        status, got = _raw(cfg, recorder, world, temp, "POST", f"/v1/work/{world.owner}/messages",
                           _send_body(world, "sink", "temp-over", "one too many"), step="W4 over budget (status)")
        expect.equals((status, (got or {}).get("error")), (429, "budget_exhausted"), "§6: HTTP 429")
        expect.equals(store.envelope("temp-over"), None, "no envelope past the allowance")
        expect.equals(store.work_files(), before, "nothing written")

        # revoke on the host, with the shipped command
        history = store.envelopes()
        rc, listed, err = _call(temp, "work", "list", world.owner, step="W4 temp list (before revoke)")
        expect.equals(rc, 0, f"the binding works before revocation: {err[-300:]}")
        revoked = _json_line(world.store.stack.operator_cs("collaboration", "revoke", temp.binding_id).stdout)
        expect.equals(revoked["active"], False, "the host reports the binding revoked")
        rc, payload, err = _call(temp, "work", "list", world.owner, step="W4 temp list (after revoke)")
        _refused(rc, payload, "binding_revoked", expect, "a read through a revoked binding")
        rc, payload, err = _send(world, temp, "sink", "after-revoke", "late", step="W4 temp send after revoke")
        _refused(rc, payload, "binding_revoked", expect, "a send through a revoked binding")
        status, got = _raw(cfg, recorder, world, temp, "POST", f"/v1/work/{world.owner}/messages",
                           _send_body(world, "sink", "after-revoke", "late"), step="W4 revoked (status)")
        expect.equals((status, (got or {}).get("error")), (403, "binding_revoked"), "§6: HTTP 403")
        expect.equals(store.envelope("after-revoke"), None, "no envelope after revocation")
        expect.equals(store.work_files(), before, "the work store is unchanged by the revoked attempts")
        expect.equals(store.envelopes(), history, "the messages sent before the revocation are kept")

    def test_05_no_lifecycle_authority_is_conferred(self, world: World, cfg, recorder, expect):
        """W5 (§5): after the exchanges the molecules' lifecycle state is untouched, a
        collaboration scope cannot nucleate/tackle/done, and lifecycle-shaped routes
        under ``/v1/work`` do not exist.
        """
        run_round_trip(world, expect)
        remote, pilot, store = world.parties["remote"], world.parties["pilot"], world.store
        # one more full cycle inside this test, so the claim is "after an exchange"
        key = "absence-1"
        rc, sent, err = _send(world, remote, "pilot", key, "does this evolve anything?", step="W5 send")
        expect.equals(rc, 0, f"send: {err[-300:]}")
        _inbox(world, pilot, peek=False, step="W5 pull")
        _ack(world, pilot, key, sent["envelope"]["payload_digest"], step="W5 ack")
        expect.equals(store.stages(key), ["admitted", "delivery_attempted", "consumed"], "the exchange happened")

        # routes that would carry lifecycle authority do not exist
        owner = world.owner
        for method, path in (
            ("POST", f"/v1/work/{owner}/evolve"),
            ("POST", f"/v1/work/{owner}/complete"),
            ("POST", f"/v1/work/{owner}/collapse"),
            ("POST", f"/v1/work/{owner}/nucleate"),
            ("POST", f"/v1/work/{owner}/tackle"),
            ("POST", f"/v1/work/{owner}/messages/{key}/evolve"),
            ("POST", f"/v1/work/{owner}/messages/{key}/complete"),
            ("DELETE", f"/v1/work/{owner}"),
        ):
            status, body = _raw(cfg, recorder, world, remote, method, path, {}, step=f"W5 {method} {path.split('/v1/')[1][:40]}")
            expect.truthy(status in (404, 405), f"{method} {path} is refused as absent (observed HTTP {status})")
        # a collaboration scope confers no lifecycle verb through the routes that do exist
        for verb, args in (
            ("nucleate", ("molecule", "nucleate", "task-work", "--topic", "from a message")),
            ("tackle", ("molecule", "tackle", world.molecules["sink"])),
            ("done", ("molecule", "done", world.molecules["sink"], "--reason", "not mine")),
        ):
            rc, payload, err = remote.cli.json(*args, step=f"W5 collaborator {verb}")
            expect.equals(rc, 4, f"the collaborator's scopes do not include {verb} (403 -> exit 4)")
        # state is unchanged
        after = store.lifecycle(list(world.molecules.values()))
        expect.equals(after, world.lifecycle_before,
                      "molecule state, event logs, the molecule set, the tenant's branches/worktrees and tmux "
                      "are exactly what they were before any message was sent")
