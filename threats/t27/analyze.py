#!/usr/bin/env python3
"""Parse sim-cli console logs into per-report time-series CSVs and (for an
A/B pair) print how the partitioned run diverges from the baseline.

Stdlib only. Usage:
    python3 analyze.py LOG [LOG ...]              # write <log>.csv for each
    python3 analyze.py --ab FULL EU_NA           # also print a diff table

The periodic reports (every 60 slots) are the data source; we key every
metric on its slot number and emit one tidy row per report.
"""
import re
import sys
import csv

ANSI = re.compile(r"\x1b\[[0-9;]*m")

# (column, regex) — all matched against the de-ANSI'd full text, keyed to the
# nearest preceding "slot N" header by position.
PATTERNS = {
    "monitor_txs":       re.compile(r"monitor\.txs:\s*(\d+) entries"),
    "mempool":           re.compile(r"mempool:\s*(\d+) entries"),
    "queued_messages":   re.compile(r"queued messages:\s*(\d+)"),
    "pending_age_s":     re.compile(r"average age of the pending transactions is ([\d.]+)s"),
    "ebs_generated":     re.compile(r"(\d+) EB\(s\) were generated"),
    "ebs_expired":       re.compile(r"(\d+) out of \d+ EBs expired"),
    "txs_in_eb":         re.compile(r"(\d+) out of \d+ transaction\(s\) were included in at least one EB"),
    "vote_bundles":      re.compile(r"There were (\d+) bundle\(s\) of votes"),
    "ebs_below_quorum":  re.compile(r"(\d+) out of \d+ EB\(s\) did not reach the vote threshold"),
    "l1_endorsed":       re.compile(r"(\d+) L1 block\(s\) had a Leios endorsement"),
    "txs_leios_ref":     re.compile(r"(\d+) tx\(s\) \([^)]*\) were referenced by a Leios endorsement"),
    "wrong_eb":          re.compile(r"WrongEB: (\d+)"),
    "t_to_eb_s":         re.compile(r"average of ([\d.]+)s \(stddev [\d.]+\) to be included in an EB"),
    "t_to_block_s":      re.compile(r"average of ([\d.]+)s \(stddev [\d.]+\) to be included in a block"),
}
SLOT_HDR = re.compile(r"stats at slot (\d+)")
PART = re.compile(r"Network partition '([^']+)' (activated|healed): (\d+) edge")


def parse(path):
    text = ANSI.sub("", open(path, encoding="utf-8", errors="replace").read())
    # Split into report blocks by slot header position.
    headers = [(m.start(), int(m.group(1))) for m in SLOT_HDR.finditer(text)]
    rows = {}
    for i, (pos, slot) in enumerate(headers):
        end = headers[i + 1][0] if i + 1 < len(headers) else len(text)
        block = text[pos:end]
        row = rows.setdefault(slot, {"slot": slot})
        for col, rx in PATTERNS.items():
            m = rx.search(block)
            if m and col not in row:
                row[col] = m.group(1)
    events = [(int(SLOT_HDR.search(text[:m.start()].rsplit("Slot ", 1)[0] + "")  # noqa
                   or 0) if False else slot_at(text, m.start()), m.group(2), int(m.group(3)))
              for m in PART.finditer(text)]
    return [rows[s] for s in sorted(rows)], events


def slot_at(text, pos):
    """Slot number of the most recent 'Slot N has begun' before pos."""
    m = None
    for m in re.finditer(r"Slot (\d+) has begun", text[:pos]):
        pass
    return int(m.group(1)) if m else 0


def write_csv(rows, out):
    cols = ["slot"] + [c for c in PATTERNS]
    with open(out, "w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=cols)
        w.writeheader()
        for r in rows:
            w.writerow(r)
    print(f"wrote {out}  ({len(rows)} reports)")


def diff_table(full, eu, events):
    win = [s for s, op, _ in events]
    span = f"{min(win)}–{max(win)}" if win else "n/a"
    print(f"\nPartition window (eu-na): slots {span}")
    print(f"\n{'slot':>5} | {'mempool (full→eu)':>22} | {'pending_age (full→eu)':>24} | "
          f"{'vote_bundles (full→eu)':>24} | {'below_quorum':>12}")
    fb = {r['slot']: r for r in full}
    for r in eu:
        s = r['slot']; b = fb.get(s, {})
        def g(d, k): return d.get(k, '—')
        print(f"{s:>5} | {g(b,'mempool')+'→'+g(r,'mempool'):>22} | "
              f"{g(b,'pending_age_s')+'→'+g(r,'pending_age_s'):>24} | "
              f"{g(b,'vote_bundles')+'→'+g(r,'vote_bundles'):>24} | "
              f"{g(b,'ebs_below_quorum')+'→'+g(r,'ebs_below_quorum'):>12}")


if __name__ == "__main__":
    args = sys.argv[1:]
    if args[:1] == ["--ab"]:
        full_rows, _ = parse(args[1]); write_csv(full_rows, args[1] + ".csv")
        eu_rows, eu_ev = parse(args[2]); write_csv(eu_rows, args[2] + ".csv")
        diff_table(full_rows, eu_rows, eu_ev)
    else:
        for p in args:
            rows, ev = parse(p)
            write_csv(rows, p + ".csv")
            if ev:
                print("  partition events:", [(s, op) for s, op, _ in ev])
