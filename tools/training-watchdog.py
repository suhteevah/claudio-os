#!/usr/bin/env python3
"""Training watchdog — summarize the current state of a QLoRA run.

Reads trainer_state.json from the latest checkpoint under models/, prints a
one-screen summary (loss curve, ETA, last step, GPU mem) suitable for piping
into a Telegram/Discord ping. Exit code:
  0 = training healthy / done
  1 = no run found
  2 = run looks stalled (no new steps in --stall-mins minutes)
  3 = run looks failed (loss spiked or NaN)

Usage:
  python tools/training-watchdog.py                 # 7B run by default
  python tools/training-watchdog.py --run 1.5b
  python tools/training-watchdog.py --stall-mins 15
  python tools/training-watchdog.py --json          # machine-readable
"""

import argparse
import json
import sys
import time
from pathlib import Path

REPO_ROOT = Path(__file__).parent.parent
MODELS_DIR = REPO_ROOT / "models"


def find_latest_checkpoint(run_glob: str) -> Path | None:
    """Find the highest checkpoint-N directory under models/<run_glob>*/."""
    candidates = []
    for run_dir in MODELS_DIR.glob(f"{run_glob}*"):
        if not run_dir.is_dir():
            continue
        for ckpt in run_dir.glob("checkpoint-*"):
            if (ckpt / "trainer_state.json").exists():
                try:
                    step = int(ckpt.name.split("-")[1])
                    candidates.append((step, ckpt))
                except (IndexError, ValueError):
                    continue
    if not candidates:
        return None
    candidates.sort()
    return candidates[-1][1]


def load_state(ckpt: Path) -> dict:
    with open(ckpt / "trainer_state.json", "r", encoding="utf-8") as f:
        return json.load(f)


def summarize(state: dict, ckpt: Path) -> dict:
    """Pull the interesting numbers out of trainer_state.json."""
    log_history = state.get("log_history", [])
    losses = [(e["step"], e["loss"]) for e in log_history if "loss" in e]
    last_loss = losses[-1][1] if losses else None
    first_loss = losses[0][1] if losses else None
    min_loss = min((l for _, l in losses), default=None)

    cur_step = state.get("global_step", 0)
    max_steps = state.get("max_steps", 0)
    epoch = state.get("epoch", 0.0)
    num_epochs = state.get("num_train_epochs", 0)

    pct = (cur_step / max_steps * 100.0) if max_steps else 0.0

    # NaN / spike detection
    nan = last_loss is not None and (last_loss != last_loss)  # NaN check
    spiked = (
        first_loss is not None
        and last_loss is not None
        and last_loss > first_loss * 3.0
    )

    mtime = (ckpt / "trainer_state.json").stat().st_mtime
    age_secs = time.time() - mtime

    return {
        "checkpoint": str(ckpt.relative_to(REPO_ROOT)),
        "step": cur_step,
        "max_steps": max_steps,
        "pct": pct,
        "epoch": epoch,
        "num_epochs": num_epochs,
        "first_loss": first_loss,
        "last_loss": last_loss,
        "min_loss": min_loss,
        "nan": nan,
        "spiked": spiked,
        "age_secs": age_secs,
        "n_log_entries": len(losses),
    }


def format_human(s: dict) -> str:
    age_min = s["age_secs"] / 60
    age_str = f"{age_min:.1f}m ago" if age_min < 60 else f"{age_min/60:.1f}h ago"
    lines = [
        f"checkpoint: {s['checkpoint']}",
        f"progress:   step {s['step']}/{s['max_steps']} ({s['pct']:.1f}%)  epoch {s['epoch']:.2f}/{s['num_epochs']}",
        f"loss:       first={s['first_loss']:.4f}  min={s['min_loss']:.4f}  last={s['last_loss']:.4f}"
        if s["last_loss"] is not None
        else "loss:       (no log entries yet)",
        f"updated:    {age_str}",
    ]
    if s["nan"]:
        lines.append("STATUS:     FAILED — NaN loss")
    elif s["spiked"]:
        lines.append("STATUS:     FAILED — loss spiked >3x initial")
    return "\n".join(lines)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--run", default="claudio-coder-7b", help="run name prefix under models/")
    ap.add_argument("--stall-mins", type=float, default=15.0, help="minutes without checkpoint update = stalled")
    ap.add_argument("--json", action="store_true", help="emit JSON instead of human summary")
    args = ap.parse_args()

    ckpt = find_latest_checkpoint(args.run)
    if ckpt is None:
        msg = f"no checkpoint found for run '{args.run}' under {MODELS_DIR}"
        print(json.dumps({"error": msg}) if args.json else msg)
        sys.exit(1)

    state = load_state(ckpt)
    s = summarize(state, ckpt)

    if args.json:
        print(json.dumps(s, indent=2))
    else:
        print(format_human(s))

    if s["nan"] or s["spiked"]:
        sys.exit(3)
    if s["age_secs"] > args.stall_mins * 60 and s["pct"] < 100.0:
        if not args.json:
            print(f"STATUS:     STALLED — no update in {args.stall_mins:.0f}m")
        sys.exit(2)
    sys.exit(0)


if __name__ == "__main__":
    main()
