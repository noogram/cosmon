# Independent reproduction mission (LLM-as-judge, null context)

You are a fresh cosmon worker with **no access** to any prior analysis of
cosmon-cli v0.2.1, no access to the external tester's report, and no access to
the bench that scored these issues. Work only from the source tree in front of
you and from first principles.

Your task: independently reproduce and score the following six candidate
issues against the **v0.2.1** source tree provided to you. For each, decide a
verdict **from scratch**:

- `RED` — you reproduced the defect (bad behaviour observed / signature found)
- `GREEN` — you looked and the defect does not reproduce on this tree
- `INCONCLUSIVE` — you could not run the discriminating step; say what you were
  missing (do **not** guess a pass)

The six candidates (investigate each independently — do not assume the framing
is correct; if the described defect is mis-stated, say so and score what you
actually find):

1. **cs verify** — does `cs verify` on a freshly created molecule pass or fail?
   Is there any writer/verifier discriminator mismatch, or is verification a
   hash chain?
2. **build deps** — does a from-source Linux/glibc build need any native
   system packages beyond the Rust toolchain? Which, and why?
3. **DAG orphan** — if a worker dies mid-flight during `cs run` over a DAG,
   does the DAG make progress, stall, or re-run completed nodes?
4. **local/ollama adapter** — does the local adapter book a mission "completed"
   without checking that real output was produced?
5. **paper cuts** — are there first-contact defects in the generated worker
   prompt / persona files (bad URLs, hard-coded absolute paths, raw usage
   dumps)?
6. **claude adapter** — what argv does cosmon actually build to launch a Claude
   worker? Interactive TUI or headless one-shot? Which permission mode?

## Output contract

Return a JSON object with exactly this shape (the bench merges your
`judge_verdict` into the report as a second opinion):

```json
{
  "unit_under_test": "v0.2.1",
  "rows": [
    {"id": "issue-1-cs-verify",     "judge_verdict": "RED|GREEN|INCONCLUSIVE", "reason": "..."},
    {"id": "issue-2-build-deps",    "judge_verdict": "...", "reason": "..."},
    {"id": "issue-3-dag-orphan",    "judge_verdict": "...", "reason": "..."},
    {"id": "issue-4-local-ollama",  "judge_verdict": "...", "reason": "..."},
    {"id": "issue-5-paper-cuts",    "judge_verdict": "...", "reason": "..."},
    {"id": "issue-6-claude-adapter","judge_verdict": "...", "reason": "..."}
  ]
}
```

The judge exists so that a human can re-run this analysis reproducibly and so
the before/after fix delta is scored by something without the bench's priors.
