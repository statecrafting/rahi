# Governed artifact reads

The compiled artifacts under `.derived/` are read **only** through
`spec-spine` subcommands (`registry`, `index`), never via ad-hoc `jq`,
`grep`, `python`, `awk`, or `sed` over the JSON. Typed reads make schema
drift fail at the deserializer with a clean error instead of silently
encoding stale assumptions.

Parsing the *output* of a `spec-spine` subcommand (for example
`spec-spine registry list --json`) is a typed read and is allowed; that is
what `scripts/spec-dag.sh` and `/next` do.
