#!/usr/bin/env python3
"""Run C5 batch-parallel `tblgen check` A/B timing and retain raw samples as JSON.

Compares the pre-C5 serial binary against the C5 binary at --jobs 1/2/4/auto
(and the serial binary itself as the reference). Every case first proves the
stdout/stderr/exit contract is byte-identical across all configurations,
then interleaved timed samples record wall/CPU/RSS plus /proc-derived
thread and fd counts.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import select
import statistics
import subprocess
import sys
import tempfile
import threading
import time
from time import perf_counter_ns, time_ns

sys.path.insert(0, str(Path(__file__).resolve().parent))
from run_cli_timing import (  # noqa: E402
    drop_caches,
    git_value,
    input_fingerprint,
    parse_time_file,
    run_plain,
    sha256_bytes,
    sha256_file,
)

POLL_INTERVAL_S = 0.005
CLK_TCK = os.sysconf("SC_CLK_TCK") if hasattr(os, "sysconf") else 100


def read_available(fd) -> bytes:
    try:
        return os.read(fd, 1 << 20)
    except BlockingIOError:
        return b""


def proc_cpu_ticks(pid: int) -> tuple[int, int] | None:
    """(utime, stime) in clock ticks from /proc/<pid>/stat."""
    try:
        stat = Path(f"/proc/{pid}/stat").read_text()
    except (FileNotFoundError, ProcessLookupError):
        return None
    # fields after the comm (which may contain spaces/parens): state is field 3
    rest = stat.rsplit(")", 1)[-1].split()
    # rest[0] = state (field 3); utime = field 14 -> rest[11], stime = field 15 -> rest[12]
    try:
        return int(rest[11]), int(rest[12])
    except (IndexError, ValueError):
        return None


def process_group_pids(pgid: int) -> list[int]:
    pids = []
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit():
            continue
        try:
            stat = (entry / "stat").read_text()
            pgrp = int(stat.rsplit(")", 1)[-1].split()[2])
        except (OSError, ValueError, IndexError):
            continue
        if pgrp == pgid:
            pids.append(int(entry.name))
    return pids


def poll_resources(stop: threading.Event, samples: dict) -> None:
    """Sample /proc for every process in the child's group until told to stop."""
    pgid = samples["pgid"]
    best = {"total": 0, "user": 0, "system": 0}
    while not stop.is_set():
        pids = process_group_pids(pgid)
        threads = fds = 0
        ticks_user = ticks_system = 0
        alive = False
        for pid in pids:
            try:
                status = Path(f"/proc/{pid}/status").read_text()
                m = re.search(r"^Threads:\s+(\d+)", status, re.M)
                if m:
                    threads += int(m.group(1))
                fds += len(os.listdir(f"/proc/{pid}/fd"))
                alive = True
            except (FileNotFoundError, ProcessLookupError, PermissionError):
                continue
            t = proc_cpu_ticks(pid)
            if t:
                ticks_user += t[0]
                ticks_system += t[1]
        if alive:
            samples["threads"] = max(samples["threads"], threads)
            samples["fds"] = max(samples["fds"], fds)
            # group-wide cumulative cpu: tblgen dominates; /usr/bin/time adds a
            # few ms. Track the max total seen (processes exit between polls).
            total = ticks_user + ticks_system
            if total > best["total"]:
                best.update(total=total, user=ticks_user, system=ticks_system)
        time.sleep(POLL_INTERVAL_S)
    samples["cpu_ticks_user"] = best["user"]
    samples["cpu_ticks_system"] = best["system"]


def jobs_flag(jobs: str | None) -> list[str]:
    # "auto" means the binary default (no flag); None = serial binary default
    if jobs is None or jobs == "auto":
        return []
    return [f"--jobs={jobs}"]


def run_sample(binary: Path, args: list[str], cache: str, drop: bool,
               jobs: str | None) -> dict:
    if drop:
        drop_caches()
    full_args = [str(binary), "check", *jobs_flag(jobs), *args]
    with tempfile.TemporaryDirectory(prefix="tblgen-c5-time-") as td:
        time_out = Path(td) / "time.txt"
        timed = ["/usr/bin/time", "-v", "-o", str(time_out), *full_args]
        start = perf_counter_ns()
        proc = subprocess.Popen(
            timed,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env={**os.environ, "LC_ALL": "C"},
            start_new_session=True,
        )
        os.set_blocking(proc.stdout.fileno(), False)
        os.set_blocking(proc.stderr.fileno(), False)
        samples = {"threads": 0, "fds": 0, "pgid": proc.pid}
        stop = threading.Event()
        poller = threading.Thread(target=poll_resources, args=(stop, samples))
        poller.start()
        out_chunks, err_chunks = [], []
        while proc.poll() is None:
            ready, _, _ = select.select([proc.stdout, proc.stderr], [], [], POLL_INTERVAL_S)
            for stream in ready:
                chunk = read_available(stream.fileno())
                if chunk:
                    (out_chunks if stream is proc.stdout else err_chunks).append(chunk)
        # drain whatever remains after exit
        for _ in range(100):
            ready, _, _ = select.select([proc.stdout, proc.stderr], [], [], 0.01)
            if not ready:
                break
            for stream in ready:
                chunk = read_available(stream.fileno())
                if chunk:
                    (out_chunks if stream is proc.stdout else err_chunks).append(chunk)
        stop.set()
        poller.join(timeout=2)
        wall_ns = perf_counter_ns() - start
        stdout = b"".join(out_chunks)
        stderr = b"".join(err_chunks)
        times = parse_time_file(time_out)
        proc_cpu_ns = int(round(
            (samples.get("cpu_ticks_user", 0) + samples.get("cpu_ticks_system", 0))
            * 1_000_000_000 / CLK_TCK))
    sample = {
        "binary": binary.name,
        "jobs": jobs if jobs is not None else "default",
        "cache": cache,
        "wall_ns": wall_ns,
        "stdout_bytes": len(stdout),
        "stdout_sha256": sha256_bytes(stdout),
        "stderr_bytes": len(stderr),
        "stderr_sha256": sha256_bytes(stderr),
        "exit_code": proc.returncode,
        "threads_max": samples["threads"],
        "fds_max": samples["fds"],
        "proc_cpu_ns": proc_cpu_ns,
        **times,
    }
    return sample


def equivalence_run(binary: Path, args: list[str], jobs: str | None) -> dict:
    cmd = [str(binary), "check", *jobs_flag(jobs), *args]
    r = run_plain(cmd)
    return {
        "binary": binary.name,
        "jobs": jobs if jobs is not None else "default",
        "exit_code": r["returncode"],
        "stdout_sha256": sha256_bytes(r["stdout"]),
        "stderr_sha256": sha256_bytes(r["stderr"]),
        "stdout_bytes": len(r["stdout"]),
        "stderr_bytes": len(r["stderr"]),
    }


def median(values):
    return statistics.median(values) if values else None


def summarize(samples: list[dict]) -> dict:
    groups = {}
    for s in samples:
        key = (s["binary"], s["jobs"], s["cache"])
        groups.setdefault(key, []).append(s)
    out = {}
    for (binary, jobs, cache), rows in sorted(groups.items()):
        out[f"{binary}:jobs={jobs}:{cache}"] = {
            "runs": len(rows),
            "wall_ns_median": median([r["wall_ns"] for r in rows]),
            "cpu_ns_median": median([r["cpu_ns"] for r in rows]),
            "max_rss_bytes_median": median([r["max_rss_bytes"] for r in rows]),
            "proc_cpu_ns_median": median([r["proc_cpu_ns"] for r in rows]),
            "threads_max_median": median([r["threads_max"] for r in rows]),
            "fds_max_median": median([r["fds_max"] for r in rows]),
            "exit_code": rows[0]["exit_code"],
            "stdout_sha256": rows[0]["stdout_sha256"],
        }
    return out


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-bin", type=Path, required=True,
                        help="pre-C5 serial tblgen binary")
    parser.add_argument("--new-bin", type=Path, required=True,
                        help="C5 tblgen binary (supports --jobs)")
    parser.add_argument("--root", type=Path, required=True,
                        help="project root passed to tblgen check")
    parser.add_argument("--repo", type=Path, help="tblgen repo for fingerprints")
    parser.add_argument("--corpus-repo", type=Path)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--batch-size", type=int, default=50)
    parser.add_argument("--cold", type=int, default=2)
    parser.add_argument("--hot", type=int, default=7)
    parser.add_argument("--warmup", type=int, default=2)
    args = parser.parse_args()

    base_bin = args.base_bin.resolve()
    new_bin = args.new_bin.resolve()
    root = args.root.resolve()
    for b in (base_bin, new_bin):
        if not b.is_file():
            parser.error(f"binary not found: {b}")
    if not root.is_dir():
        parser.error(f"root is not a directory: {root}")
    if args.batch_size < 1 or args.cold < 0 or args.hot < 1 or args.warmup < 0:
        parser.error("invalid run counts")

    # preliminary report selects the small batch (first N files by report
    # order) and the single most diagnostic-dense file
    preliminary = json.loads(run_plain([str(base_bin), "check", str(root)])["stdout"])
    report_files = [r["file"] for r in preliminary["files"]]
    fingerprint = input_fingerprint(root, report_files)
    batch = report_files[: args.batch_size]
    diag = sorted(
        preliminary["files"],
        key=lambda r: (-(len(r.get("errors", [])) + len(r.get("warnings", []))), r["file"]),
    )[0]["file"]
    cases = {
        "full": {"args": [str(root)], "kind": "directory"},
        "batch": {"args": batch, "kind": "files"},
        "single": {"args": [diag], "kind": "files"},
    }

    # equivalence: every configuration must produce identical bytes/exit
    configs = [("base", None), ("c5", "1"), ("c5", "2"), ("c5", "4"), ("c5", "auto")]
    # note: for the base binary jobs is always None; "auto" on c5 = default
    results = {}
    for case_name, case in cases.items():
        eq = [equivalence_run(base_bin if b == "base" else new_bin, case["args"], j)
              for b, j in configs]
        ref = eq[0]
        mismatches = [e for e in eq[1:]
                      if not (e["exit_code"] == ref["exit_code"]
                              and e["stdout_sha256"] == ref["stdout_sha256"]
                              and e["stderr_sha256"] == ref["stderr_sha256"])]
        if mismatches:
            raise RuntimeError(f"equivalence failed for {case_name}: {mismatches}")

        samples = []
        # warmup: every config once, hot cache
        for b, j in configs:
            run_sample(base_bin if b == "base" else new_bin, case["args"], "hot", False, j)
        for rep in range(max(args.hot, args.cold)):
            order = list(range(len(configs)))
            if rep % 2:
                order.reverse()
            for idx in order:
                b, j = configs[idx]
                binary = base_bin if b == "base" else new_bin
                if rep < args.cold:
                    samples.append(run_sample(binary, case["args"], "cold", True, j))
                elif rep < args.cold + args.warmup:
                    samples.append(run_sample(binary, case["args"], "hot", False, j))
                else:
                    samples.append(run_sample(binary, case["args"], "hot", False, j))

        expected = ref["stdout_sha256"]
        for s in samples:
            if (s["stdout_sha256"] != expected
                    or s["exit_code"] != ref["exit_code"]):
                raise RuntimeError(f"timed output mismatch for {case_name}: {s}")

        results[case_name] = {
            "args_count": len(case["args"]),
            "args_kind": case["kind"],
            "equivalence": eq,
            "samples": samples,
            "summary": summarize(samples),
        }
        print(f"case {case_name}: {len(samples)} samples", file=sys.stderr, flush=True)

    meta = {
        "generated_at_unix_ns": time_ns(),
        "platform": platform.platform(),
        "python": sys.version.split()[0],
        "cpu_count": os.cpu_count(),
        "base_bin": str(base_bin),
        "base_bin_sha256": sha256_file(base_bin),
        "new_bin": str(new_bin),
        "new_bin_sha256": sha256_file(new_bin),
        "root": str(root),
        "tblgen_commit": git_value(args.repo, "rev-parse", "HEAD") if args.repo else None,
        "corpus_commit": git_value(args.corpus_repo, "rev-parse", "HEAD") if args.corpus_repo else None,
        "corpus_branch": git_value(args.corpus_repo, "rev-parse", "--abbrev-ref", "HEAD") if args.corpus_repo else None,
        "input_fingerprint": fingerprint,
        "single_case_file": diag,
        "config": {
            "configs": [f"{b}:jobs={j or 'default'}" for b, j in configs],
            "batch_size": args.batch_size,
            "cold_runs": args.cold,
            "hot_runs": args.hot,
            "warmup_runs": args.warmup,
        },
    }
    payload = {"meta": meta, "cases": results}
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
    print(f"wrote {args.out}", file=sys.stderr, flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
