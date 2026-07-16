# Cross-provider review is opt-in

ADR-147's provider-diversity witness applies when a fleet or spore explicitly
requests it. It is off by default: ordinary development does not create a
cross-provider review requirement merely because a task is labelled critical.

Enable it for an entire fleet with:

```toml
[review]
cross_provider = true
# Optional pin for the independent refuter.
reviewer_adapter = "openai"
```

For a shareable polymer, put the same declaration under `[spore.review]`:

```toml
[spore.review]
cross_provider = true
reviewer_adapter = "anthropic"
```

At nucleation or germination Cosmon projects `needs-review` and
`needs-review-cross-provider` onto every affected molecule; the optional pin is
recorded as `reviewer-adapter:<name>`. These review marks are monotone under
RR-SAFE-2, so normal worker tag mutation cannot strip the requirement before an
independent refuter clears it. The refuter must still satisfy ADR-147's resolved
provider-family diversity rule; an adapter name alone is not evidence of
independence.
