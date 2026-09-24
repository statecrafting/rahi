#!/usr/bin/env python3
"""Spec 043 revision 4, D-P17: a deterministic model of bearer admission,
revocation pruning and the wall clock.

A model, not a product test: it encodes the admission rules of 043 B-6 and
B-6b and three pruning policies, and checks them against fixed timelines and
a bounded grid. No rahi code runs. Exit 0 means every expectation held,
including the expected FAILURE of revision 3's rule on the post-prune clock
rollback counterexample (the negative control).

Policies:
  rev2       prune at revoked_at + V(L*), L* the largest L declared so far
  rev3       prune at revoked_at + V(L_max); nothing else
  retain     rows are never pruned (revision 4, P-9 option (i))
  watermark  prune at revoked_at + V(L_max), and in the same atomic step
             raise a persisted watermark W to the pruning instant; admission
             refuses every token while now < W (revision 4, P-9 option (ii))
  watermark-volatile  as watermark, but W is lost at restart (negative
             control for the persistence requirement)

Usage: python3 clock_model.py   (stdlib only, no network, no files kept)
"""

import json
import os
import sys
import tempfile

LEEWAY = 60
L_MAX = 86_400
U64_MAX = 2**64 - 1


def v(lifetime):
    """denylist_ttl: V(L) = L + 2 * LEEWAY (038 D-7)."""
    return lifetime + 2 * LEEWAY


def checked_add(a, b):
    s = a + b
    return None if s > U64_MAX else s


def checked_sub(a, b):
    return None if b > a else a - b


class Cell:
    """The durable state the bearer reads, plus a restart that keeps only
    what the policy persists (serialized through a real file)."""

    def __init__(self, policy, lifetime, floor=None):
        self.policy = policy
        self.lifetime = lifetime
        self.l_star = lifetime
        self.floor = floor
        self.jti = {}  # jti -> revoked_at
        self.sub = {}  # sub -> revoked_at
        self.watermark = 0
        self.last_prune = None

    def set_lifetime(self, lifetime):
        assert 10 <= lifetime <= L_MAX, "manifest validation (rahi-kernel)"
        self.lifetime = lifetime
        self.l_star = max(self.l_star, lifetime)

    def revoke_jti(self, jti, now):
        self.jti[jti] = now

    def revoke_sub(self, sub, now):
        self.sub[sub] = max(self.sub.get(sub, 0), now)

    def prune(self, now):
        if self.policy == "retain":
            return 0
        horizon = v(self.l_star) if self.policy == "rev2" else v(L_MAX)
        before = len(self.jti) + len(self.sub)
        # One atomic step: the deletes and the watermark raise commit together.
        self.jti = {k: r for k, r in self.jti.items() if r + horizon > now}
        self.sub = {k: r for k, r in self.sub.items() if r + horizon > now}
        if self.policy.startswith("watermark"):
            self.watermark = max(self.watermark, now)
        self.last_prune = now
        return before - len(self.jti) - len(self.sub)

    def restart(self, workdir):
        path = os.path.join(workdir, "cell.json")
        kept = {
            "floor": self.floor,
            "jti": self.jti,
            "sub": self.sub,
            "lifetime": self.lifetime,
            "l_star": self.l_star,
        }
        if self.policy == "watermark":
            kept["watermark"] = self.watermark
        tmp = path + ".tmp"
        with open(tmp, "w") as f:
            json.dump(kept, f)
            f.flush()
            os.fsync(f.fileno())
        os.replace(tmp, path)
        with open(path) as f:
            back = json.load(f)
        fresh = Cell(self.policy, back["lifetime"], back["floor"])
        fresh.l_star = back["l_star"]
        fresh.jti = {k: int(r) for k, r in back["jti"].items()}
        fresh.sub = {k: int(r) for k, r in back["sub"].items()}
        fresh.watermark = back.get("watermark", 0)
        return fresh

    def admit(self, tok, now):
        """043 B-6 and B-6b admission, as refusals. Returns (admitted, why)."""
        iat, exp = tok.get("iat"), tok["exp"]
        for claim in (iat, exp):
            if claim is not None and not 0 <= claim <= U64_MAX:
                return False, "claim outside u64"
        if self.policy.startswith("watermark") and now < self.watermark:
            return False, "clock below the prune watermark"
        if iat is None:
            return False, "no iat"
        span = checked_sub(exp, iat)
        if span is None:
            return False, "exp < iat"
        if span > L_MAX:
            return False, "exp - iat > L_max"
        if span > self.lifetime:
            return False, "exp - iat > L"
        limit = checked_add(now, LEEWAY)
        if limit is None:
            return False, "overflow"
        if iat > limit:
            return False, "iat in the future"
        if self.floor is not None and iat <= self.floor:
            return False, "iat at or before the floor"
        exp_leeway = checked_add(exp, LEEWAY)
        if exp_leeway is None:
            return False, "overflow"
        if exp_leeway <= now:
            return False, "expired"
        if tok["jti"] in self.jti:
            return False, "jti revoked"
        r = self.sub.get(tok["sub"])
        if r is not None and iat <= r:
            return False, "subject revoked"
        return True, "admitted"


FAILURES = []


def expect(label, got, want):
    ok = got == want
    print(f"  {'ok  ' if ok else 'FAIL'} {label}: {got} (expected {want})")
    if not ok:
        FAILURES.append(label)


def counterexample(workdir):
    print("1. Post-prune clock rollback (the revision 4 counterexample)")
    print("   floor=0, iat=100, exp=700, L=600, revoke=200, prune=86,721, now=300")
    tok = {"iat": 100, "exp": 700, "jti": "t1", "sub": "u1"}
    for policy, want in [
        ("rev3", True),
        ("retain", False),
        ("watermark", False),
    ]:
        c = Cell(policy, 600, floor=0)
        expect(f"{policy}: admitted at 150 before revocation", c.admit(tok, 150)[0], True)
        c.revoke_jti("t1", 200)
        expect(f"{policy}: refused at 250 after revocation", c.admit(tok, 250)[0], False)
        c.prune(86_721)
        c = c.restart(workdir)
        got, why = c.admit(tok, 300)
        expect(f"{policy}: admitted at now=300 after prune and restart ({why})", got, want)
    # The subject form of the same rollback.
    c = Cell("rev3", 600, floor=0)
    c.revoke_sub("u1", 200)
    c.prune(86_721)
    expect("rev3: subject-revoked token admitted at 300 after prune", c.admit(tok, 300)[0], True)
    c = Cell("retain", 600, floor=0)
    c.revoke_sub("u1", 200)
    c.prune(86_721)
    expect("retain: subject-revoked token refused at 300", c.admit(tok, 300)[0], False)


def lifetime_increase(workdir):
    print("2. Lifetime increase after a prune (043 7.4 item 4's timeline)")
    tok = {"iat": 0, "exp": 3600, "jti": "t2", "sub": "u2"}
    for policy, want in [("rev2", True), ("rev3", False), ("retain", False), ("watermark", False)]:
        c = Cell(policy, 600)
        c.revoke_jti("t2", 100)
        c.prune(820)
        c = c.restart(workdir)
        c.set_lifetime(3600)
        expect(f"{policy}: admitted at 1000 after L raised to 3600", c.admit(tok, 1000)[0], want)
    c = Cell("retain", 600)
    expect("L=600 refuses exp - iat = 3600", c.admit(tok, 50)[1], "exp - iat > L")


def boundaries():
    print("3. Admission boundaries: floor, lifetime, future iat, overflow")
    c = Cell("retain", 600, floor=1000)
    now = 1200
    expect("iat == floor refused", c.admit({"iat": 1000, "exp": 1500, "jti": "a", "sub": "s"}, now)[0], False)
    expect("iat == floor + 1 admitted", c.admit({"iat": 1001, "exp": 1500, "jti": "a", "sub": "s"}, now)[0], True)
    expect("no iat refused", c.admit({"iat": None, "exp": 1500, "jti": "a", "sub": "s"}, now)[1], "no iat")
    expect("exp < iat refused", c.admit({"iat": 1100, "exp": 1099, "jti": "a", "sub": "s"}, now)[1], "exp < iat")
    expect("exp - iat == L admitted", c.admit({"iat": 1100, "exp": 1700, "jti": "a", "sub": "s"}, now)[0], True)
    expect("exp - iat == L + 1 refused", c.admit({"iat": 1100, "exp": 1701, "jti": "a", "sub": "s"}, now)[1], "exp - iat > L")
    expect("iat == now + 60 admitted", c.admit({"iat": now + 60, "exp": now + 600, "jti": "a", "sub": "s"}, now)[0], True)
    expect("iat == now + 61 refused", c.admit({"iat": now + 61, "exp": now + 600, "jti": "a", "sub": "s"}, now)[1], "iat in the future")
    big = Cell("retain", L_MAX, floor=0)
    expect("exp = u64::MAX, iat = 0 refused", big.admit({"iat": 0, "exp": U64_MAX, "jti": "a", "sub": "s"}, now)[1], "exp - iat > L_max")
    expect("iat = u64::MAX, exp = 0 refused", big.admit({"iat": U64_MAX, "exp": 0, "jti": "a", "sub": "s"}, now)[1], "exp < iat")
    expect(
        "iat = exp = u64::MAX refused without overflow",
        big.admit({"iat": U64_MAX, "exp": U64_MAX, "jti": "a", "sub": "s"}, now)[1],
        "iat in the future",
    )
    expect(
        "exp + 60 past u64::MAX refuses (checked addition, overflow refuses)",
        big.admit({"iat": U64_MAX - 100, "exp": U64_MAX - 50, "jti": "a", "sub": "s"}, U64_MAX - 70)[1],
        "overflow",
    )
    expect("negative iat refused", big.admit({"iat": -1, "exp": 10, "jti": "a", "sub": "s"}, now)[1], "claim outside u64")
    expect("hard maximum: exp - iat = L_max + 1 refused even if L were raised past it",
           Cell("retain", L_MAX).admit({"iat": 10, "exp": 10 + L_MAX + 1, "jti": "a", "sub": "s"}, 20)[1],
           "exp - iat > L_max")


def restart_guard(workdir):
    print("4. Restart of the persisted watermark (and its negative control)")
    tok = {"iat": 100, "exp": 700, "jti": "t3", "sub": "u3"}
    for policy, want in [("watermark", False), ("watermark-volatile", True)]:
        c = Cell(policy, 600)
        c.revoke_jti("t3", 200)
        c.prune(86_721)
        c = c.restart(workdir)
        expect(f"{policy}: now=300 after restart", c.admit(tok, 300)[0], want)
    c = Cell("watermark", 600)
    c.prune(86_721)
    c = c.restart(workdir)
    expect("watermark: a fresh token at now=86,721 admitted", c.admit({"iat": 86_700, "exp": 87_000, "jti": "n", "sub": "n"}, 86_721)[0], True)
    print("   cost of the watermark: a forward clock error followed by a prune")
    c = Cell("watermark", 600)
    c.prune(1_000_000)  # the clock jumped forward, then was corrected
    c = c.restart(workdir)
    got, why = c.admit({"iat": 390, "exp": 900, "jti": "f", "sub": "f"}, 400)
    expect(f"watermark: every token refused at now=400 < W=1,000,000 ({why})", got, False)


def grid(workdir):
    print("5. Bounded grid: a revoked token is never admitted again")
    lifetimes = [600, 3600, L_MAX]
    times = [0, 50, 100, 200, 300, 700, 1_000, 5_000, 86_400, 86_520, 86_721, 90_000, 200_000]
    counts = {p: [0, 0] for p in ("rev3", "retain", "watermark")}
    rev3_only_under_rollback = True
    for lt in lifetimes:
        for iat in times:
            for span in (0, 1, lt // 2, lt):
                exp = iat + span
                for revoke in times:
                    if revoke < iat - LEEWAY:
                        continue  # 043 B-6: a revoked token has iat <= revoked_at + LEEWAY
                    for prune_at in times:
                        if prune_at < revoke:
                            continue
                        for now in times:
                            tok = {"iat": iat, "exp": exp, "jti": "g", "sub": "g"}
                            for policy in counts:
                                c = Cell(policy, lt)
                                c.revoke_jti("g", revoke)
                                c.prune(prune_at)
                                admitted = c.admit(tok, now)[0]
                                counts[policy][0] += 1
                                if admitted:
                                    counts[policy][1] += 1
                                    if policy == "rev3" and now >= prune_at:
                                        rev3_only_under_rollback = False
    for policy, (runs, bad) in counts.items():
        print(f"   {policy}: {runs} cases, {bad} admitted a revoked token")
    expect("retain never admits a revoked token", counts["retain"][1], 0)
    expect("watermark never admits a revoked token", counts["watermark"][1], 0)
    expect("rev3 admits some revoked token (the defect exists)", counts["rev3"][1] > 0, True)
    expect("rev3 admits one only when the clock is below its last prune", rev3_only_under_rollback, True)


def main():
    with tempfile.TemporaryDirectory(prefix="rahi-043-clock-") as workdir:
        counterexample(workdir)
        lifetime_increase(workdir)
        boundaries()
        restart_guard(workdir)
        grid(workdir)
    if FAILURES:
        print(f"\n{len(FAILURES)} expectation(s) failed: {FAILURES}")
        return 1
    print("\nall expectations held (revision 3's rule failed where expected)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
