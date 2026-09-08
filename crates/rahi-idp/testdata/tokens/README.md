# Recorded access token claims

The claim sets rauthy puts in an access token, for the three cases spec 025
turns on. `person.json` is an authorization code grant for a human being;
`service.json` is a client credentials grant, where rauthy maps `sub` to the
client id and there is no person behind it (B-4); `other-resource.json` is a
perfectly valid token from the same issuer that was minted for somewhere
else, which is the one B-3 refuses on the audience alone (RFC 8707, D-2).

They are claim sets and not whole JWTs on purpose. A recorded signature would
be a signature under a key nobody holds, and asserting on it would prove
nothing about the verification path; the tests sign these payloads with the
RSA key under `../oidc/` and check them against the key set that same stub
publishes, so the RS256 path under test is the real one.

`https://cell.example.com` is a placeholder: the test rewrites it to the
origin the stub was booted on, which is how one recorded document serves
every run. The timestamps are relative to the stub's fixed clock
(`T0 = 1767225600`), so a token here is current at the moment a test starts
and the tests that need an expired or premature one edit the claim they mean.

They are fixtures, not evidence. A run against a real rauthy is AC-2's, and
`tests/bearer.rs` opens it when `RAHI_TEST_RAUTHY_URL` names the origin of a
running one.

## Running AC-2 against a real rauthy

AC-2 is a live run, not a fixture. `tests/bearer.rs` skips it unless
`RAHI_TEST_RAUTHY_URL` names the origin of a running rauthy (D-10), and when
it is set the test does every step the criterion asks for: it registers a
client through rauthy's dynamic registration endpoint, completes
authorization code with PKCE against a loopback redirect, presents the
resulting access token to a scope-gated route, and watches the same token
refused by a route requiring a scope it lacks.

Setting the URL is the opt in, so the other four variables are then required
rather than defaulted: a run that has opted in and cannot reach rauthy is a
failure, not a skip.

| variable | what it is |
|---|---|
| `RAHI_TEST_RAUTHY_URL` | the origin rauthy is published on, for example `http://localhost:8080` |
| `RAHI_TEST_RAUTHY_REG_TOKEN` | `dynamic_clients.reg_token`, which B-7's default `token` mode requires |
| `RAHI_TEST_RAUTHY_API_KEY` | an admin API key as `name$secret`, for the two provisioning calls |
| `RAHI_TEST_RAUTHY_USER` | the person who completes the login, default `admin@localhost` |
| `RAHI_TEST_RAUTHY_PASSWORD` | that person's password |

### The rauthy this expects

Verified against `ghcr.io/sebadob/rauthy:0.36.0` over plain `http` on port
8080. Four settings are load bearing:

- `[server] scheme = 'http'`, `port_http = 8080`, `pub_url = 'localhost:8080'`,
  so the issuer rauthy publishes is `http://localhost:8080/auth/v1/`, which is
  what `ISSUER_PATH` builds and `Discovery::parse` requires (spec 021 D-10).
- `[dynamic_clients] enable = true` with a `reg_token`, which is what B-7's
  `token` mode means.
- `[dynamic_clients] rate_limit_sec = 0`. rauthy rate limits registration per
  IP for sixty seconds by default, so with the default a second run inside
  that window answers `429` and the criterion is not reproducible.
- `[bootstrap] password_plain` and an `api_key` / `api_key_secret` pair, so a
  test can log in and provision without a browser.

`[encryption]`, `[cluster]`, and a `[webauthn]` block with `rp_id`,
`rp_origin`, and `rp_name` are required by rauthy itself; it panics at boot
without them.

### What the test provisions, and why rauthy needs it

Two calls before the flow, both with the admin API key:

1. `POST /auth/v1/scopes` creates `api:read` and `api:write`. A scope that
   does not exist cannot be granted, and AC-2 needs one the token carries and
   one it does not.
2. `PUT /auth/v1/clients/{id}` sets `access_token_alg` to `RS256` and
   `allowed_resources` to this cell's origin. Both are refusals otherwise:
   rauthy signs with `EdDSA` by default and this chassis verifies only RS256
   (B-3), and rauthy answers `invalid_target` to a `resource` parameter that
   is not on the client's allow list, so without it no token can carry the
   audience B-3 demands.

The login itself needs two things a browser would do invisibly: an anonymous
session from `POST /auth/v1/oidc/session`, whose cookie and CSRF token the
authorize call echoes, and a solved proof of work from `POST /auth/v1/pow`.
The challenge is `version:difficulty:expiry:salt:hash:` and the answer
appends the smallest counter whose SHA-256 opens with `difficulty` zero bits.
rauthy also refuses a login with an empty `User-Agent`.

### Running it

```sh
docker run -d --name rauthy -p 8080:8080 \
  -v "$PWD/config.toml:/app/config.toml:ro" -v rauthy-data:/app/data \
  ghcr.io/sebadob/rauthy:0.36.0

RAHI_TEST_RAUTHY_URL=http://localhost:8080 \
RAHI_TEST_RAUTHY_REG_TOKEN=<reg token> \
RAHI_TEST_RAUTHY_API_KEY='<name>$<secret>' \
RAHI_TEST_RAUTHY_PASSWORD=<admin password> \
cargo test -p rahi-idp --locked --test bearer
```

The token that comes back is the one the fixtures above imitate: RS256 over
rauthy's own key, `iss` of `http://localhost:8080/auth/v1/`, `aud` holding
both the client id and this cell's origin, and `scope` of `openid api:read`.
