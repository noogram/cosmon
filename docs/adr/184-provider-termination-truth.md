# ADR-184 — Provider termination truth crosses the AgentLoop port

**Status:** Accepted (2026-10-02).  
**Decider:** Noogram.  
**Amends:** [ADR-102](102-cosmon-agent-harness-and-agentloop-port.md).  
**Tracks:** issue #151, unit W1.

## Context

ADR-102 makes each provider schema responsible for decoding one native response
into a harness turn. The original `Turn::Stop(String)` retained text but erased
why the provider stopped. An output limit, refusal, unknown reason, missing
terminator, and ordinary completion therefore became indistinguishable. Arm A
also accepted any streamed data frame as a completed response and could dispatch
a tool envelope even when an explicit non-success reason ended the response.

The tool counter had a related ambiguity. It counted attempts before dispatch,
while a structured shell result could report a nonzero exit or timeout inside an
outer `Ok`. A read, a failed command, and a successful path-checked mutation
were all reduced to the same integer. That integer is useful for budgets but is
not evidence that a deliverable exists.

## Decision

The AgentLoop port carries a typed terminal response containing both retained
assistant text and one of five dispositions: normal, output limit, refusal,
incomplete, or unknown with the native reason retained. The legacy string stop
variant remains a normal-stop compatibility constructor for existing in-process
callers.

Provider decoders apply these rules:

1. An explicit output limit, refusal, incomplete response, or unknown reason
   dominates any accompanying tool fields. Those tools are not dispatched.
2. A complete native tool envelope with a normal-stop label remains compatible.
3. Arm A streaming requires a completion sentinel or a recognized reason, a
   valid assistant-data stream, and complete tool identity before dispatch.
   Comments and keepalives do not invalidate a response; malformed assistant
   data does.
4. Whole-response and Arm B decoders map their native reasons through the same
   provider-neutral disposition set. Missing reasons are incomplete, never
   normal by absence.

The loop outcome retains the final disposition and classifies returned tool
results as succeeded, failed, or uncertain. A shell timeout is uncertain; a
nonzero exit is failed; zero exit is successful execution but is not successful
deliverable evidence. Only successful path-checked file mutations increment the
successful-effect telemetry count. Reads and control tools may succeed without
claiming a mutation.

All counts are telemetry. Neither a normal stop nor a successful effect grants
lifecycle authority. Formula-bound acceptance and step progression remain a
separate boundary.

## Consequences

Partial text survives limits and refusals for diagnosis. Incomplete streams and
partial tools fail closed without inventing a provider error or executing an
uncertain call. Interactive callers can distinguish a typed terminal yield while
legacy scripted callers retain source compatibility.

The successful-effect count is intentionally narrow. It cannot recognize useful
effects hidden inside arbitrary shell commands, and it does not attempt to prove
task completion. Later acceptance work must inspect declared artifacts and
verification rather than widening this telemetry into an oracle.
