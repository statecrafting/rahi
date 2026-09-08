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

## The AC-2 recipe, and where it stops

Every OAuth step AC-2 asks for has been run green by hand against
`ghcr.io/sebadob/rauthy:0.36.0`, so the criterion is not blocked on the
authorization server being unavailable. Booted over plain `http` on port 8080
with `[dynamic_clients] enable = true` and a `reg_token`, a `[bootstrap]`
admin password, `[encryption]` keys, and a `[webauthn]` block, rauthy answers:

1. `POST /auth/v1/clients_dyn` with the registration token registers a public
   client whose one redirect URI is a loopback address (B-7, RFC 7591).
2. `PUT /auth/v1/clients/{id}` as the admin adds a scope to it and sets
   `allowed_resources` to this cell's origin. Rauthy denies a resource
   indicator by default, so without this step no token can carry the audience
   B-3 requires.
3. `POST /auth/v1/oidc/authorize` with a solved proof of work and an S256
   challenge answers `202` and a `Location` carrying the code; the proof of
   work is a SHA-256 with a leading-zero-bit count, which `ring` already
   computes here.
4. `POST /auth/v1/oidc/token` with the verifier and `resource` returns an
   access token whose `aud` holds both the client id and this cell's origin,
   and whose `scope` is the one that was granted.

What stops there is the handshake before all of it. Spec 021 B-2 fixes this
cell's issuer at `<public_url>/auth/v1`, and `Discovery::parse` holds the
published document to it exactly. Rauthy builds its issuer as
`{scheme}://{pub_url}/auth/v1/` (`src/data/src/rauthy_config.rs`) and offers
no setting that removes the trailing slash, so the two never compare equal:

```text
the discovery document is issued by http://localhost:8080/auth/v1/ and this
cell's issuer is http://localhost:8080/auth/v1: the document belongs to
another deployment
```

The same one-character difference would refuse every real token on the `iss`
check in `ResourceServer::validate`. It is a contradiction in a spec this one
only extends, so spec 025 reports it rather than reconciling it from here.
