# Recorded discovery documents

`rauthy.json` is rauthy 0.36's `openid-configuration` as it is served for a
cell whose public URL is `https://cell.example.com`, with the endpoints
rewritten to that origin because rauthy builds them from its own configured
public URL (spec 021 B-3).

The other two are that document with one thing wrong, which is what spec 021
FR-002 asks for: `wrong-issuer.json` is issued by another deployment, and
`missing-jwks-uri.json` has no signing keys to name.

They are fixtures, not evidence: an integration run against a real rauthy is
FR-004's, and it is skipped unless `RAHI_TEST_RAUTHY` names a binary.
