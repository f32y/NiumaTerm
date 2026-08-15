"""Compare the last-request cache rate (what the UI shows) with the whole-turn rate."""

import glob
import json
import os

home = os.path.expanduser("~")
files = sorted(
    glob.glob(os.path.join(home, ".claude/projects/*/*.jsonl")), key=os.path.getmtime
)[-40:]

turns = []  # each: list of (inp, cw, cr)
for path in files:
    cur = []
    seen = set()
    with open(path, encoding="utf-8", errors="replace") as fh:
        for line in fh:
            try:
                rec = json.loads(line)
            except Exception:
                continue
            if rec.get("isSidechain"):
                continue
            msg = rec.get("message") or {}
            if rec.get("type") == "user":
                content = msg.get("content")
                is_tool_result = isinstance(content, list) and any(
                    isinstance(c, dict) and c.get("type") == "tool_result"
                    for c in content
                )
                if not is_tool_result:
                    if cur:
                        turns.append(cur)
                    cur = []
                continue
            usage = msg.get("usage")
            if not isinstance(usage, dict):
                continue
            uid = rec.get("uuid")
            if uid in seen:
                continue
            seen.add(uid)
            triple = (
                usage.get("input_tokens") or 0,
                usage.get("cache_creation_input_tokens") or 0,
                usage.get("cache_read_input_tokens") or 0,
            )
            if sum(triple):
                cur.append(triple)
    if cur:
        turns.append(cur)

turns = [t for t in turns if t]
print("turns:", len(turns), "requests:", sum(len(t) for t in turns))


def rate(rows):
    cr = sum(r[2] for r in rows)
    den = sum(sum(r) for r in rows)
    return 100.0 * cr / den if den else 0.0


last = [rate([t[-1]]) for t in turns]
whole = [rate(t) for t in turns]
first = [rate([t[0]]) for t in turns]


def show(vals, label):
    v = sorted(vals)
    n = len(v)
    q = lambda p: v[min(n - 1, int(p * n))]
    print(
        f"{label}: p10={q(.1):5.1f} p50={q(.5):5.1f} p90={q(.9):5.1f} "
        f"rounds to 100%: {sum(x >= 99.5 for x in v) * 100 / n:.0f}%"
    )


show(first, "first request of turn ")
show(last, "last  request of turn ")
show(whole, "whole turn aggregate  ")

multi = [i for i, t in enumerate(turns) if len(t) > 1]
print(f"multi-request turns: {len(multi)}/{len(turns)}")
if multi:
    d = sorted(last[i] - whole[i] for i in multi)
    print(
        f"last-minus-turn gap (pp): p50={d[len(d) // 2]:.1f} p90={d[int(.9 * len(d))]:.1f} max={d[-1]:.1f}"
    )

# What the miss actually costs, in tokens, on turns the UI calls ~100%.
near100 = [t for i, t in enumerate(turns) if last[i] >= 99.5]
if near100:
    miss = sorted(sum(r[0] + r[1] for r in t) for t in near100)
    print(
        f"turns the UI shows as 100%: {len(near100)}; uncached+written tokens in them: "
        f"p50={miss[len(miss) // 2]}, p90={miss[int(.9 * len(miss))]}, max={miss[-1]}"
    )
