# Vetoer Briefing — cosmon Constitution

**Version:** 0.1 (draft) · **Date:** 2026-04-13 · **Audience:** prospective external vetoer

You have been approached to serve as one of three external vetoers for the
constitution of a software project called **cosmon**. This document explains
what that role is, why it exists, and what we ask of you. Read it once. If
it does not convince you that the role is worth your time, decline. We would
rather recruit slowly than recruit poorly.

---

## 1. What you are being asked to do

The operator has built a system for orchestrating multiple AI coding agents
working in parallel on the same codebase. The system is called cosmon. It is
reaching a scale where informal conventions no longer suffice: the patterns
that shape what the system *can* and *cannot* do are becoming load-bearing.

We are writing a **Constitution** — a short, versioned document that states,
as axioms, what cosmon must do, what it must never do, and how those axioms
can be changed. Each axiom is paired with a continuous-integration test that
fails the build if the axiom is violated. The Constitution is code-enforced
policy.

**Your role:** before the Constitution ships (v1.0), and before any future
amendment, two of three external vetoers must approve. Without that approval,
the Constitution does not take effect. You are not asked to build, debug, or
maintain anything. You are asked to read policy and judge whether it is sound.

Without external approval, the Constitution is unfalsifiable self-justification:
the author is also the judge, which is theater. You are the witness that makes
the constitutional commitment real.

---

## 2. Why external review is structurally necessary

A formal argument, made simple.

Kurt Gödel proved that no sufficiently expressive formal system can prove its
own consistency from within. The system can derive its theorems, but whether
the axioms themselves are free of contradiction must be checked from outside.

Software constitutions inherit a version of the same problem. The author of a
constitution is in no position to certify that it serves anyone other than the
author. Every such document, absent external review, is structurally a
self-portrait of the interests of whoever wrote it. This is not a statement
about the author's character. It is a statement about the geometry of
self-reference.

External vetoers are the out-of-system check. Your reading is what makes the
axioms falsifiable — what turns a self-description into a commitment.

You do not need to share the author's technical worldview. The more
independent your perspective, the more signal your approval carries.

---

## 3. The 2-of-3 model

Three vetoers are recruited independently. Any two must approve for a
Constitution or amendment to ship. One dissenter blocks nothing; two
dissenters block everything.

- **Resists single-point capture.** A single vetoer who is co-opted,
  pressured, or simply inattentive cannot alone wave a flawed axiom through.
- **Resists rubber-stamping.** A single approver faces social pressure to
  approve. Three independent readers, each knowing the others may reject,
  have cover to say no.
- **Tolerates absence.** Any single vetoer can step down, take leave, or
  disagree without halting the project.

The three of you do not need to coordinate. You will not be told who the
others are unless they consent to it. You vote independently.

---

## 4. What cosmon is (no jargon)

Modern AI coding agents are good at focused work but brittle at coordination.
Run two of them on the same codebase and they overwrite each other. Run ten
and the operator loses track of which agent is doing what, which crashed,
which is waiting, which finished.

Cosmon is a small command-line tool that gives each agent a persistent
identity, an isolated git working copy, and a typed lifecycle — nucleated,
active, completed, collapsed. JSON files on disk are the source of truth.
There is no server, no database, no daemon. The operator composes agents
the way one composes Unix commands.

As the system matured, recurring patterns crystallized: templates for agent
teams, recipes for multi-step work, conventions for observability. These
patterns are not code features; they are the grammar the system runs on.
Writing them down as a Constitution — and enforcing them in CI — is how we
keep the grammar from drifting as cosmon grows. Your role is to certify that
the grammar is honest: that the axioms describe what is actually good for
users and the broader ecosystem, not merely what is convenient for the
author.

---

## 5. What you review (and what you do not)

**You review:**

- The **Constitution document** itself — target 500 lines, 8 axioms, each
  stated in plain language with a rationale and failure mode.
- The **CI test suite** that enforces each axiom — one test per axiom,
  written so that a non-programmer can read the test's description and
  understand what it asserts.
- The **amendment procedure** — how the Constitution can be changed, who
  must approve, how disagreements are resolved.

**You do not review:**

- The cosmon implementation (Rust source code).
- Individual pull requests, bug fixes, or feature additions.
- The operator's commercial strategy.

You review **policy**, not **code**. If an axiom is well-formed, enforceable,
and defensible, the implementation is not your concern.

---

## 6. What we ask of you

- **Initial review:** approximately **2 hours of reading** and **1 hour of
  discussion** (asynchronous or synchronous, your choice) within **14 days**
  of receiving the Constitution draft.
- **Each future amendment:** approximately **30 minutes**. Amendments are
  expected to be rare (target: fewer than 4 per year).
- **Freedom to ask anything.** You may request any clarification, any
  evidence, any rationale. There is no "out of scope" question.
- **Freedom to reject.** You may reject without writing a justification. A
  silent veto is a valid veto. A reasoned veto is more useful but not
  required.
- **Freedom to step down.** You may resign at any time without explanation.
  We will replace you on the same 2-of-3 basis.

We will not ask you to sign non-disclosure agreements. The Constitution and
all review materials will be open-source.

---

## 7. What you bring (self-assessment)

Before you accept, check yourself against these criteria. If you cannot
answer yes to all four, decline — for your sake and ours.

1. **Rigor.** You have formal training or professional experience in
   reviewing specifications, standards, or formal arguments. You will read
   closely and push back on sloppiness.
2. **Independence.** You hold no commercial stake in cosmon. You are not
   employed by the operator, nor in a position where approval or rejection
   would materially affect a working relationship.
3. **Time.** You can commit the 3 hours for the initial review within 14
   days, and 30 minutes per subsequent amendment.
4. **Intellectual honesty.** You are willing to reject the Constitution — or
   any amendment — without apology, even after investing time reading it.
   Sunk cost is not a reason to approve.

---

## 8. Compensation and acknowledgment

**Today:** no monetary compensation. Your name (or a pseudonym of your
choice) will be credited in `CONSTITUTION.md` and in any publication,
talk, or paper derived from this work, with language of your choosing.

**If cosmon becomes commercially successful:** the operator commits, in
writing, to open a retroactive compensation discussion with each vetoer who
served during the pre-commercial period. This may take the form of equity,
a one-time payment, or other arrangements by mutual agreement. The
commitment is binding in spirit and will be made binding in contract when
commercial structure is formalized.

You are not being asked to trust a verbal promise alone. The commitment
will appear in the Constitution itself, under an axiom that the amendment
procedure cannot remove without unanimous vetoer consent.

---

## 9. Glossary

Terms you will encounter in the Constitution, in plain language.

- **Molecule.** One unit of work tracked by cosmon — a task, an idea, a
  decision, a bug report. Each molecule has an identity, a lifecycle, and a
  directory on disk. Think: one issue in an issue tracker, but with a typed
  state machine.
- **Polymer.** A chain of molecules linked by dependencies. Think: a
  workflow, a DAG of tasks where some must finish before others start.
- **Galaxy / Universe.** A single cosmon project (one `.cosmon/` directory
  = one galaxy). A multiverse is several galaxies observed together from
  the operator's workstation.
- **Multiverse.** The set of all cosmon-managed projects on a given
  machine, observable through one command.
- **Constitution.** The short, versioned policy document you are being
  asked to vet. Axioms + CI tests + amendment procedure.
- **P_external.** Shorthand for "probability that an external reviewer
  approves." A design metric: any change whose P_external is low is a
  change that probably should not ship.
- **IFBDD.** "It From Bit, Doing Design" — shorthand for the project's
  commitment to let the system observe and curate itself (the patterns
  and tools that shape work are themselves objects in the system).

Footnote for the curious: the project internally uses a physics-inspired
vocabulary (nucleate, evolve, collapse, entangle, observe) for molecule
lifecycle operations. You will not need it to do your work. It is mentioned
here only so you recognize it if you see it in referenced material.

---

## 10. Optional reading

This briefing is sufficient to vet the Constitution. The reading list below
is for depth only.

- **cosmon README** — 5-minute elevator pitch of the tool.
- **THESIS.md** — long-form rationale for the project's design choices.
  Optional; read only if you want context on *why* certain axioms exist.
- **Selected chronicles** (3–5 short dated notes) — moments in the project's
  history that illuminate a principle. The operator will select and send
  these with the Constitution draft.

---

## Next step

If you are interested, reply to the operator with one of:

- **Yes**, with any questions or conditions.
- **Not now**, with optional reason (scheduling, conflict of interest, etc.).
- **No**, with or without reason.

If you suggest another candidate in your reply, we are grateful. Recruiting
a strong panel matters more than filling seats quickly.

Thank you for reading this far.
