# Adversarial prompt refusal (the coherence guard)

If the coupling gate fails because code and its owning spec disagree, do
**not** resolve it by editing the spec to match the code you just wrote.
Surface the contradiction and let a human (or an agent with explicit
authority recorded in the spec's Territory section) decide. Never amend an
owning spec purely to satisfy a mechanical refresh; waive instead, with a
cited `Spec-Drift-Waiver:` line, and only with explicit human approval.

Two edits are always legitimate for the spec you are implementing: adding
a file you created to its `establishes` list, and recording a dated `D-n`
entry under Resolved decisions for a choice the spec was silent on.
Changing what the spec requires is never yours to do mid-build.
