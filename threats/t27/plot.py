#!/usr/bin/env python3
"""Plot a baseline-vs-partition A/B from the CSVs that analyze.py emits.

    pip install matplotlib pandas
    python3 plot.py FULL.csv PARTITION.csv --window 299 899 --out ab.png

Draws a 4-panel time series (pending-tx age, EBs below quorum, vote bundles,
mempool) with the partition window shaded and the heal instant marked.
"""
import argparse
import csv


def load(path):
    cols = {}
    with open(path) as f:
        for row in csv.DictReader(f):
            for k, v in row.items():
                cols.setdefault(k, []).append(float(v) if v not in ("", None) else float("nan"))
    return cols


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("full_csv")
    ap.add_argument("partition_csv")
    ap.add_argument("--window", nargs=2, type=float, metavar=("START", "STOP"),
                    default=[299, 899], help="partition window slots to shade")
    ap.add_argument("--label", default="eu-isolate", help="partition series label")
    ap.add_argument("--out", default="ab.png")
    args = ap.parse_args()

    import matplotlib
    matplotlib.use("Agg")
    import matplotlib.pyplot as plt

    full = load(args.full_csv)
    part = load(args.partition_csv)
    w0, w1 = args.window

    panels = [
        ("pending_age_s", "Pending-tx age (s)", "the stall: txs not finalizing"),
        ("ebs_below_quorum", "EBs below vote threshold (cumulative)", "certification failures"),
        ("vote_bundles", "Vote bundles (cumulative)", "vote supply"),
        ("mempool", "Mempool size (entries)", "backlog"),
    ]
    fig, axes = plt.subplots(len(panels), 1, figsize=(10, 11), sharex=True)
    for ax, (col, ylab, sub) in zip(axes, panels):
        ax.plot(full["slot"], full[col], label="baseline (no partition)", color="#1f77b4", lw=1.8)
        ax.plot(part["slot"], part[col], label=args.label, color="#d62728", lw=1.8)
        ax.axvspan(w0, w1, color="red", alpha=0.08)
        ax.axvline(w1, color="red", ls="--", lw=1, alpha=0.6)
        ax.set_ylabel(ylab, fontsize=9)
        ax.set_title(sub, fontsize=8, loc="left", color="#555")
        ax.grid(alpha=0.25)
    axes[0].legend(fontsize=9, loc="upper left")
    axes[0].annotate("partition\nwindow", xy=((w0 + w1) / 2, axes[0].get_ylim()[1] * 0.85),
                     ha="center", fontsize=8, color="#a00")
    axes[-1].set_xlabel("slot")
    fig.suptitle("T27 §S2 — complete EU isolation vs baseline (1500 slots)", fontsize=12)
    fig.tight_layout(rect=[0, 0, 1, 0.98])
    fig.savefig(args.out, dpi=130)
    print(f"wrote {args.out}")


if __name__ == "__main__":
    main()
