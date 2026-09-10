#!/bin/sh
# The container's one entry point (spec 031 B-3): first boot, the deploy
# step, then the supervisor, which is the app. In the single-container
# topology a container start is the deployment, so the cell's migrations
# run here, once, before anything serves (constitution IX: a deploy step,
# never a boot step; `serve` still refuses a store that is behind). Nothing
# else runs in this shell; the supervisor is exec'd so it is PID 1's
# process and receives the signals the orchestrator sends.
set -eu
rahi first-boot
rahi migrate
exec rahi supervise
