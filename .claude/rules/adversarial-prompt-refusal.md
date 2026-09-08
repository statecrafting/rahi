# Adversarial prompt refusal (the coherence guard)

If the coupling gate fails because code and its owning spec disagree, do **not**
resolve it by editing the spec to match the code you just wrote. Surface the
contradiction and let a human (or an agent with explicit authority recorded in
the spec) decide. Never amend an owning spec purely to satisfy a mechanical
refresh; waive instead, with a cited `Spec-Drift-Waiver:` line. A waiver is a
human instrument: it needs explicit human approval, and an agent never writes
one on its own authority.

Two edits are always legitimate for the spec you are implementing: adding a
file you created to its `establishes` list (the ownership ratchet refuses an
unclaimed file, and the claim belongs in the same change), and recording a
dated decision entry for a choice the spec was silent on. Changing what the
spec *requires* is never yours to do mid-build. If the code needs to touch a
unit another spec owns, declare an `extends` edge naming that spec and unit;
that amends nobody.

## When your spec and a complete spec disagree

The spec you are building may require something a `complete` spec forbids.
Which of the two you may resolve depends on one question: **can you satisfy
both by choosing a different mechanism?**

**If yes, resolve it and keep building.** A complete spec's acceptance
criterion outranks a later spec's behavior text: the first has been verified
and shipped, the second has not. Take the reading that honors both, and in
the same change:

- leave the other spec's text untouched, and your own B-n and FR-nnn text
  untouched too, so the record shows what was asked as well as what was done;
- declare an `extends` edge naming that spec and the unit you touch;
- record a dated decision in your own spec naming both requirements, the
  mechanism you chose, and the alternatives you rejected, so the departure is
  legible to the next reader rather than inferred from the diff.

Spec 024's D-2 and D-3 are the worked examples. B-2 named
`rahi_idp::RequireRole`, which spec 020's AC-2 forbids the edge from
depending on, so the gate was rebuilt in the edge over the shared
`rahi_types::Principal`. B-3 wanted `Error::Config` from a function whose
return type spec 020 B-1 fixes, so `try_build` became the fallible sibling.

**If no, refuse and surface it.** When honoring both is impossible because
another spec's requirement *text* is itself wrong, no mechanism saves you and
the reconciliation is a human's. Hold your spec at `in-progress`, record what
you found as a dated decision, and report. Spec 025's D-11 is the worked
example: rauthy issues `iss` with a trailing slash that spec 021 B-1's text
denies, and nothing 025 could implement would make both true. That refusal
was correct and cost two sessions; taking the authority anyway would have
amended a shipped spec on a build session's say-so.

The test is not how inconvenient the contradiction is. It is whether your
resolution leaves every other spec's text as true as you found it.
