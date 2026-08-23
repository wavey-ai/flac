#!/usr/bin/env python3
"""Interleaved decode benchmark: wavey-flac against system libFLAC.

Alternates single benchmark invocations of the Rust decoder (via
examples/codec_bench) and the C reference decoder (scripts/libflac_bench.c,
compiled against Homebrew libFLAC). Alternation cancels machine-load drift,
so each side sees the same load conditions. The ffmpeg CLI is deliberately
not used: process startup dominates its wall time at these scales.

Compile the C harness first:

    target_dir=$(cargo metadata --no-deps --format-version 1 | \
        python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')
    cc -O2 $(pkg-config --cflags flac) scripts/libflac_bench.c \
        $(pkg-config --libs flac) -o "$target_dir/release/libflac_bench"

Usage:

    python3 scripts/bench_interleaved.py FILE.flac [ROUNDS] [RUNS_PER_TOOL]

ROUNDS is how many alternating pairs to run (default 5). RUNS_PER_TOOL is
the timed-run count inside each invocation (default 5).
"""

import json
import re
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent


def cargo_target_dir():
    result = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"],
        cwd=REPO,
        capture_output=True,
        text=True,
        check=True,
    )
    return Path(json.loads(result.stdout)["target_directory"])


TARGET_DIR = cargo_target_dir()
CODEC_BENCH = TARGET_DIR / "release/examples/codec_bench"
LIBFLAC_BENCH = TARGET_DIR / "release/libflac_bench"

MEDIAN_RE = re.compile(r"median ([0-9]+\.[0-9]+) s")


def median_of(values):
    ordered = sorted(values)
    middle = len(ordered) // 2
    if len(ordered) % 2 == 1:
        return ordered[middle]
    return (ordered[middle - 1] + ordered[middle]) / 2


def run_tool(binary, args):
    result = subprocess.run(
        [str(binary)] + args, capture_output=True, text=True, check=True
    )
    match = MEDIAN_RE.search(result.stderr + result.stdout)
    if not match:
        raise RuntimeError(f"no median in output of {binary}")
    return float(match.group(1))


def main():
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    flac_path = sys.argv[1]
    rounds = int(sys.argv[2]) if len(sys.argv) > 2 else 5
    runs_per_tool = int(sys.argv[3]) if len(sys.argv) > 3 else 5

    for binary in (CODEC_BENCH, LIBFLAC_BENCH):
        if not binary.exists():
            sys.exit(f"missing {binary}; build it first (see module docstring)")

    rust_medians = []
    c_medians = []
    for round_index in range(1, rounds + 1):
        # Alternate which tool goes first so ordering bias cancels too.
        if round_index % 2 == 1:
            pair = ("rust", "c")
        else:
            pair = ("c", "rust")
        times = {}
        for tool in pair:
            if tool == "rust":
                args = ["decode", flac_path, "q", str(runs_per_tool)]
            else:
                args = [flac_path, str(runs_per_tool)]
            times[tool] = run_tool(
                CODEC_BENCH if tool == "rust" else LIBFLAC_BENCH, args
            )
        rust_medians.append(times["rust"])
        c_medians.append(times["c"])
        print(
            f"round {round_index}: rust={times['rust']:.4f} s "
            f"libflac={times['c']:.4f} s"
        )

    rust_median = median_of(rust_medians)
    c_median = median_of(c_medians)
    print(f"\nrust   median-of-medians: {rust_median:.4f} s")
    print(f"libflac median-of-medians: {c_median:.4f} s")
    print(f"ratio rust/libflac: {rust_median / c_median:.2f}x")


if __name__ == "__main__":
    main()
