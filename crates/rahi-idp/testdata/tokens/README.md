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
it is skipped unless `RAHI_TEST_RAUTHY` names a binary.
