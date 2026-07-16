# Vocabulary

<!-- Absorbed from foundry colony (2026-04-13). Replace domain-specific terms. -->

| Term | Meaning |
|------|---------|
| **Kernel** | The type-checking core. TCB. <CHANGE_ME_TCB_BUDGET>-line budget. |
| **Probe** | The LLM-powered proof search. Untrusted by design. |
| **Firewall** | The crate boundary between probe and kernel. Structural, not policy. |
| **Term** | A proof term — the syntactic object the kernel checks. |
| **Context** | An ordered list of variable bindings (typing environment). |
| **Level** | A universe level — stratification preventing Girard's paradox. |
| **Judgment** | The kernel's affirmation: "term M has type A in context Gamma". |
| **Corpus** | Test suite of must-accept and must-reject proof terms. |
| **TCB** | Trusted Computing Base — code that must be correct for soundness. |
| **Golden hash** | A proof term with a known, pinned hash for semver regression testing. |
