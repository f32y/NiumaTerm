"""Probe real Claude Code / Codex session records for cache-hit accounting."""

import glob
import json
import os
import sys

home = os.path.expanduser("~")


def claude():
    files = sorted(
        glob.glob(os.path.join(home, ".claude/projects/*/*.jsonl")),
        key=os.path.getmtime,
    )[-40:]
    rows = []
    for path in files:
        with open(path, encoding="utf-8", errors="replace") as fh:
            for line in fh:
                try:
                    rec = json.loads(line)
                except Exception:
                    continue
                usage = (rec.get("message") or {}).get("usage")
                if not isinstance(usage, dict):
                    continue
                inp = usage.get("input_tokens") or 0
                cw = usage.get("cache_creation_input_tokens") or 0
                cr = usage.get("cache_read_input_tokens") or 0
                if inp + cw + cr == 0:
                    continue
                rows.append((inp, cw, cr, os.path.basename(path)))
    return rows


def codex():
    files = sorted(
        glob.glob(os.path.join(home, ".codex/sessions/*/*/*/*.jsonl")),
        key=os.path.getmtime,
    )[-40:]
    rows = []
    for path in files:
        with open(path, encoding="utf-8", errors="replace") as fh:
            for line in fh:
                if "token" not in line:
                    continue
                try:
                    rec = json.loads(line)
                except Exception:
                    continue
                payload = rec.get("payload") or rec
                info = payload.get("info") if isinstance(payload, dict) else None
                if not isinstance(info, dict):
                    continue
                for key in ("last_token_usage", "total_token_usage"):
                    u = info.get(key)
                    if isinstance(u, dict):
                        rows.append((key, u, os.path.basename(path)))
    return rows


def pct(cr, denom):
    return 100.0 * cr / denom if denom else 0.0


def hist(vals, label):
    vals = sorted(vals)
    if not vals:
        print(f"{label}: no samples")
        return
    n = len(vals)
    q = lambda p: vals[min(n - 1, int(p * n))]
    print(
        f"{label}: n={n} p10={q(.1):.1f} p50={q(.5):.1f} p90={q(.9):.1f} "
        f"max={vals[-1]:.1f} >=99%: {sum(v >= 99 for v in vals) * 100 / n:.0f}% "
        f"==100%: {sum(v >= 99.995 for v in vals) * 100 / n:.0f}%"
    )


print("=== Claude Code ===")
rows = claude()
print("samples:", len(rows))
if rows:
    app_formula = [pct(cr, inp + cw + cr) for inp, cw, cr, _ in rows]
    naive = [pct(cr, inp) for inp, cw, cr, _ in rows if inp]
    hist(app_formula, "app: cr/(inp+cw+cr)")
    hist(naive, "if inp were the whole input: cr/inp")
    print("sample rows (inp, cache_write, cache_read):")
    for r in rows[-6:]:
        print("  ", r[:3], f"-> {pct(r[2], r[0] + r[1] + r[2]):.1f}%")
    zero_write = sum(1 for i, w, c, _ in rows if w == 0)
    print(f"rows with cache_creation==0: {zero_write}/{len(rows)}")
    tiny_inp = sum(1 for i, w, c, _ in rows if i < 100)
    print(f"rows with raw input_tokens<100: {tiny_inp}/{len(rows)}")

print()
print("=== Codex ===")
crows = codex()
print("samples:", len(crows))
keys = set()
for _, u, _ in crows:
    keys |= set(u)
print("fields seen:", sorted(keys))
for want in ("last_token_usage", "total_token_usage"):
    sel = [u for k, u, _ in crows if k == want]
    if not sel:
        continue
    over = sum(
        1
        for u in sel
        if (u.get("cached_input_tokens") or 0) > (u.get("input_tokens") or 0)
    )
    print(f"{want}: n={len(sel)} cached>input rows: {over}")
    vals = [
        pct(u.get("cached_input_tokens") or 0, u.get("input_tokens") or 0)
        for u in sel
        if (u.get("input_tokens") or 0) > 0
    ]
    hist(vals, f"  {want} cached/input")
    for u in sel[-4:]:
        print("   ", {k: v for k, v in u.items() if "token" in k})
