#!/bin/sh
# The container's one entry point (spec 031 B-3): first boot, the deploy
# step, then the supervisor, which is the app. In the single-container
# topology a container start is the deployment, so the cell's migrations
# run here, once, before anything serves (constitution IX: a deploy step,
# never a boot step; `serve` still refuses a store that is behind). The same
# step adopts the cell's manifest (spec 036 B-3): a container start is the
# deployment, so a widened or narrowed ceiling is ledgered here rather than
# meeting `serve` as a refusal. Nothing else runs in this shell; the
# supervisor is exec'd so it is PID 1's process and receives the signals the
# orchestrator sends.
set -eu
rahi first-boot
rahi migrate --adopt-manifest
exec rahi supervise
