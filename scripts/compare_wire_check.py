"""Compare release builds of benches/wire_check.rs; write paired raw samples as JSON."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
from time import perf_counter_ns


def diagnostics(binary, source):
    output = subprocess.check_output([binary, "diagnostics", source])
    return len(output.splitlines()), hashlib.sha256(output).hexdigest()


def sample(binary, source, repeats):
    cmd = [binary, "time", source, str(repeats)]
    measure_rss = sys.platform == "darwin"
    if measure_rss:
        cmd = ["/usr/bin/time", "-l", *cmd]
    start = perf_counter_ns()
    p = subprocess.run(
        cmd, capture_output=True, text=True, check=True, env={**os.environ, "LC_ALL": "C"}
    )
    wall = perf_counter_ns() - start
    stats = json.loads(p.stdout)
    stats["wall_ns"] = wall
    if measure_rss:
        peak = re.search(r"^\s*(\d+)\s+maximum resident set size", p.stderr, re.MULTILINE)
        if not peak:
            raise ValueError("could not parse peak RSS from /usr/bin/time")
        stats["rss_bytes"] = int(peak[1])
    return stats


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("before", type=Path)
    parser.add_argument("after", type=Path)
    parser.add_argument("--experiment", required=True, type=Path, help="generated .tbl, not a spec")
    parser.add_argument("--corpus", type=Path, help="JSON array of .tbl paths")
    parser.add_argument("--pairs", type=int, default=8)
    parser.add_argument("--variant", nargs=2, action="append", default=[], metavar=("NAME", "BIN"))
    parser.add_argument("--case", nargs=2, action="append", default=[], metavar=("SOURCE", "REPEATS"))
    args = parser.parse_args()
    if args.pairs < 2 or args.pairs % 2:
        parser.error("--pairs must be an even number >= 2")
    with args.experiment.open(encoding="utf-8-sig") as f:
        experiment = json.load(f)
    flow = experiment.get("dataFlow") if isinstance(experiment, dict) else None
    if not isinstance(flow, dict) or not isinstance(flow.get("nodes"), dict):
        parser.error("--experiment must be generated DataFlow wire JSON, not a spec")
    bins = dict(before=args.before.resolve(), after=args.after.resolve())
    for label, binary in args.variant:
        if label in bins:
            parser.error(f"duplicate variant name: {label}")
        bins[label] = Path(binary).resolve()
    if args.pairs % len(bins):
        parser.error("--pairs must be divisible by the number of builds")
    cases = {
        "width:1": 4096,
        "width:32": 256,
        "width:256": 12,
        "width:1024": 2,
        str(args.experiment.resolve()): 100,
    }
    if args.corpus:
        cases["manifest:" + str(args.corpus.resolve())] = 1
    for source, n in args.case:
        if not n.isdecimal() or int(n) < 1:
            parser.error("case repeats must be a positive integer")
        cases[source] = int(n)
    # Rotate a balanced order: AB/BA for two, ABDC/BCAD/... for four.
    labels = list(bins)
    offsets = [0] + [(j + 1) // 2 if j % 2 else -j // 2 for j in range(1, len(bins))]
    results = {}
    for source, repeats in cases.items():
        signatures = [diagnostics(b, source) for b in bins.values()]
        if any(sig != signatures[0] for sig in signatures[1:]):
            raise SystemExit(f"ordered diagnostics differ for {source}: {signatures}")
        files, digest = signatures[0]
        for b in bins.values():
            sample(b, source, repeats)
        pairs = []
        for i in range(args.pairs):
            order = [labels[(i + offset) % len(bins)] for offset in offsets]
            pair = {label: sample(bins[label], source, repeats) for label in order}
            if not all(s["documents"] == files and s["repeats"] == repeats for s in pair.values()):
                raise ValueError("inconsistent timed input count")
            pairs.append(pair)
        results[source] = {"documents": files, "diagnostics_sha256": digest, "pairs": pairs}
        print(f"finished {source} ({files} documents)", file=sys.stderr, flush=True)
    json.dump(results, sys.stdout, indent=2)
    print()


if __name__ == "__main__":
    main()
