# Security policy

## Reporting a vulnerability

Report security issues **privately, through GitHub's private vulnerability
reporting** for this repository: open the repository's **Security** tab and
choose **Report a vulnerability**. That is the only reporting channel; there
is no security email address.

Please do not open a public issue, pull request or discussion for a suspected
vulnerability.

## What is in scope

This repository's code and the artifacts it publishes: the chassis crates
under `crates/`, the reference app under `apps/`, the container images built
from `docker/`, and the manifests under `deploy/`. Findings in a dependency
(hiqlite, rauthy, spec-spine) belong to that project's own policy; a finding
in how rahi configures or composes one is in scope here.

## Supported versions

Reports are judged against the current `main` and the latest published
release. Older releases receive no fixes.
