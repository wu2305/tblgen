#!/usr/bin/env python3
"""Run C1 segmented `tblgen check` timing and retain raw samples as JSON."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import statistics
import subprocess
import sys
import tempfile
from time import perf_counter_ns, time_ns

TIMING_PREFIX = "TBLGEN_CLI_TIMING_JSON="
CHECK_EXTENSIONS = (".tbl", ".action.ts")


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


def input_fingerprint(root: Path, report_files: list[str]) -> dict:
    paths = sorted((Path(file) for file in report_files), key=lambda p: str(p))
    h = hashlib.sha256()
    total_bytes = 0
    counts = {".tbl": 0, ".action.ts": 0}
    for path in paths:
        try:
            rel = path.relative_to(root).as_posix()
        except ValueError:
            rel = str(path)
        data = path.read_bytes()
        digest = hashlib.sha256(data).hexdigest()
        total_bytes += len(data)
        for ext in CHECK_EXTENSIONS:
            if rel.endswith(ext):
                counts[ext] += 1
                break
        h.update(rel.encode("utf-8"))
        h.update(b"\0")
        h.update(digest.encode("ascii"))
        h.update(b"\0")
    return {
        "algorithm": "sha256 over sorted relative path + per-file sha256",
        "files": len(paths),
        "bytes": total_bytes,
        "by_extension": counts,
        "sha256": h.hexdigest(),
    }


def run_plain(cmd: list[str]) -> dict:
    start = perf_counter_ns()
    p = subprocess.run(
        cmd,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env={**os.environ, "LC_ALL": "C"},
        check=False,
    )
    wall_ns = perf_counter_ns() - start
    return {
        "returncode": p.returncode,
        "stdout": p.stdout,
        "stderr": p.stderr,
        "wall_ns": wall_ns,
    }


def parse_time_file(path: Path) -> dict:
    text = path.read_text(encoding="utf-8", errors="replace")
    def number(pattern: str) -> float:
        m = re.search(pattern, text)
        if not m:
            raise ValueError(f"missing {pattern} in {path}: {text[:500]!r}")
        return float(m.group(1))

    user = number(r"User time \(seconds\): ([0-9.]+)")
    system = number(r"System time \(seconds\): ([0-9.]+)")
    rss_kb = number(r"Maximum resident set size \(kbytes\): ([0-9]+)")
    return {
        "user_ns": int(round(user * 1_000_000_000)),
        "system_ns": int(round(system * 1_000_000_000)),
        "cpu_ns": int(round((user + system) * 1_000_000_000)),
        "max_rss_bytes": int(rss_kb) * 1024,
    }


def drop_caches() -> None:
    subprocess.run(
        ["sudo", "-n", "sh", "-c", "sync; echo 3 > /proc/sys/vm/drop_caches"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
        check=True,
    )


def run_sample(kind: str, binary: Path, args: list[str], cache: str, drop: bool) -> dict:
    if drop:
        drop_caches()
    with tempfile.TemporaryDirectory(prefix="tblgen-c1-time-") as td:
        time_out = Path(td) / "time.txt"
        if kind == "tblgen":
            cmd = [str(binary), "check", *args]
        elif kind == "bench":
            cmd = [str(binary), "time", *args]
        else:
            raise ValueError(kind)
        timed_cmd = ["/usr/bin/time", "-v", "-o", str(time_out), *cmd]
        start = perf_counter_ns()
        p = subprocess.run(
            timed_cmd,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env={**os.environ, "LC_ALL": "C"},
            check=False,
        )
        wall_ns = perf_counter_ns() - start
        times = parse_time_file(time_out)
    sample = {
        "binary": kind,
        "cache": cache,
        "wall_ns": wall_ns,
        "stdout_bytes": len(p.stdout),
        "stdout_sha256": sha256_bytes(p.stdout),
        "stderr_bytes": len(p.stderr),
        "exit_code": p.returncode,
        **times,
    }
    if kind == "bench":
        stats_lines = [
            line for line in p.stderr.decode("utf-8", errors="replace").splitlines()
            if line.startswith(TIMING_PREFIX)
        ]
        if not stats_lines:
            raise ValueError("bench time run did not emit timing JSON")
        sample["bench_stats"] = json.loads(stats_lines[-1][len(TIMING_PREFIX):])
    elif p.stderr:
        # The real CLI only writes diagnostics to stderr; keep a hash for equivalence checks.
        sample["stderr_sha256"] = sha256_bytes(p.stderr)
    return sample


def run_equivalence(tblgen: Path, bench: Path, args: list[str]) -> dict:
    actual = run_plain([str(tblgen), "check", *args])
    harness = run_plain([str(bench), "report", *args])
    return {
        "exit_equal": actual["returncode"] == harness["returncode"],
        "stdout_equal": actual["stdout"] == harness["stdout"],
        "stderr_equal": actual["stderr"] == harness["stderr"],
        "exit_code": actual["returncode"],
        "stdout_bytes": len(actual["stdout"]),
        "stdout_sha256": sha256_bytes(actual["stdout"]),
        "stderr_bytes": len(actual["stderr"]),
        "stderr_sha256": sha256_bytes(actual["stderr"]),
    }


def median(values):
    return statistics.median(values) if values else None


def summarize(samples: list[dict]) -> dict:
    groups = {}
    for sample in samples:
        key = (sample["binary"], sample["cache"])
        groups.setdefault(key, []).append(sample)
    out = {}
    for (binary, cache), rows in sorted(groups.items()):
        entry = {
            "runs": len(rows),
            "wall_ns_median": median([r["wall_ns"] for r in rows]),
            "cpu_ns_median": median([r["cpu_ns"] for r in rows]),
            "max_rss_bytes_median": median([r["max_rss_bytes"] for r in rows]),
            "stdout_bytes": rows[0]["stdout_bytes"],
            "exit_code": rows[0]["exit_code"],
        }
        bench_rows = [r for r in rows if "bench_stats" in r]
        if bench_rows:
            phase_names = sorted({p for r in bench_rows for p in r["bench_stats"]["phases"]})
            entry["phase_wall_ns_median"] = {
                p: median([r["bench_stats"]["phases"][p]["wall_ns"] for r in bench_rows])
                for p in phase_names
            }
            entry["phase_cpu_ns_median"] = {
                p: median([r["bench_stats"]["phases"][p]["cpu_ns"] for r in bench_rows])
                for p in phase_names
            }
            entry["phase_alloc_bytes_median"] = {
                p: median([r["bench_stats"]["phases"][p]["alloc_bytes"] for r in bench_rows])
                for p in phase_names
            }
            entry["alloc_peak_bytes_median"] = median(
                [r["bench_stats"]["totals"]["alloc_peak_bytes"] for r in bench_rows]
            )
        out[f"{binary}:{cache}"] = entry
    return out


def choose_cases(preliminary: dict, batch_size: int) -> tuple[dict, dict]:
    rows = []
    for report in preliminary["files"]:
        errors = len(report.get("errors", []))
        warnings = len(report.get("warnings", []))
        rows.append({
            "file": report["file"],
            "scope": report.get("scope", ""),
            "errors": errors,
            "warnings": warnings,
            "total": errors + warnings,
        })
    dataflow = [r for r in rows if r["scope"] == "dataflow"]
    pool = dataflow or rows
    low = sorted(pool, key=lambda r: (r["total"], r["file"]))[:batch_size]
    high = sorted(pool, key=lambda r: (-r["total"], r["file"]))[:batch_size]
    single_low = low[0]
    single_high = high[0]

    def selection(name, rows_):
        totals = [r["total"] for r in rows_]
        return {
            "case": name,
            "scope": "dataflow" if dataflow else "all",
            "metric": "errors+warnings",
            "files": [r["file"] for r in rows_],
            "file_count": len(rows_),
            "errors": sum(r["errors"] for r in rows_),
            "warnings": sum(r["warnings"] for r in rows_),
            "total_diagnostics": sum(totals),
            "min_file_diagnostics": min(totals) if totals else None,
            "max_file_diagnostics": max(totals) if totals else None,
        }

    cases = {
        "full": {"args": None, "selection": {"case": "full", "scope": "all-checkable-files"}},
        "low_dataflow": {"args": sorted(r["file"] for r in low), "selection": selection("low_dataflow", low)},
        "high_dataflow": {"args": sorted(r["file"] for r in high), "selection": selection("high_dataflow", high)},
        "single_low_dataflow": {"args": [single_low["file"]], "selection": selection("single_low_dataflow", [single_low])},
        "single_high_dataflow": {"args": [single_high["file"]], "selection": selection("single_high_dataflow", [single_high])},
    }
    return cases, {"file_rows": len(rows), "dataflow_rows": len(dataflow)}


def find_bench(deps: Path) -> Path:
    candidates = [
        p for p in deps.glob("cli_timing-*")
        if p.is_file() and os.access(p, os.X_OK) and not p.name.endswith(".d")
    ]
    if not candidates:
        raise FileNotFoundError(f"no cli_timing bench binary under {deps}; run cargo build --release --bench cli_timing")
    return max(candidates, key=lambda p: p.stat().st_mtime)


def git_value(repo: Path, *args: str) -> str | None:
    try:
        p = subprocess.run(
            ["git", "-C", str(repo), *args],
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            check=True,
        )
        return p.stdout.strip()
    except Exception:
        return None


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tblgen", type=Path, default=Path("target/release/tblgen"))
    parser.add_argument("--bench", type=Path, help="cli_timing bench binary; auto-detected by default")
    parser.add_argument("--root", type=Path, required=True, help="project root passed to tblgen check")
    parser.add_argument("--repo", type=Path, help="tblgen repo for source fingerprint")
    parser.add_argument("--corpus-repo", type=Path, help="corpus git repo for commit fingerprint")
    parser.add_argument("--out", type=Path, required=True, help="raw JSON output path")
    parser.add_argument("--batch-size", type=int, default=50)
    parser.add_argument("--cold", type=int, default=3)
    parser.add_argument("--hot", type=int, default=5)
    parser.add_argument("--warmup", type=int, default=1)
    args = parser.parse_args()

    if args.batch_size < 1 or args.cold < 1 or args.hot < 1 or args.warmup < 0:
        parser.error("batch-size/cold/hot must be positive; warmup may be zero")

    tblgen = args.tblgen.resolve()
    bench = (args.bench or find_bench(tblgen.parent / "deps")).resolve()
    root = args.root.resolve()
    if not tblgen.is_file():
        parser.error(f"tblgen binary not found: {tblgen}")
    if not bench.is_file():
        parser.error(f"bench binary not found: {bench}")
    if not root.is_dir():
        parser.error(f"root is not a directory: {root}")

    print("running preliminary CLI report for case selection", file=sys.stderr, flush=True)
    preliminary_run = run_plain([str(tblgen), "check", str(root)])
    if preliminary_run["returncode"] not in (0, 1):
        raise RuntimeError(f"preliminary check failed: {preliminary_run['stderr'][:500]!r}")
    preliminary = json.loads(preliminary_run["stdout"])

    print("fingerprinting corpus", file=sys.stderr, flush=True)
    fingerprint = input_fingerprint(root, [r["file"] for r in preliminary["files"]])
    cases, selection_meta = choose_cases(preliminary, args.batch_size)

    binaries = {"tblgen": tblgen, "bench": bench}
    results = {}
    for case_name, case in cases.items():
        case_args = [str(root)] if case_name == "full" else case["args"]
        print(f"case {case_name}: {len(case_args)} args", file=sys.stderr, flush=True)
        equivalence = run_equivalence(tblgen, bench, case_args)
        if not (equivalence["exit_equal"] and equivalence["stdout_equal"] and equivalence["stderr_equal"]):
            raise RuntimeError(f"equivalence failed for {case_name}: {equivalence}")

        samples = []
        for i in range(args.cold):
            order = ["tblgen", "bench"] if i % 2 == 0 else ["bench", "tblgen"]
            for kind in order:
                samples.append(run_sample(kind, binaries[kind], case_args, "cold", True))
        for _ in range(args.warmup):
            for kind in ("tblgen", "bench"):
                run_sample(kind, binaries[kind], case_args, "hot", False)
        for i in range(args.hot):
            order = ["tblgen", "bench"] if i % 2 == 0 else ["bench", "tblgen"]
            for kind in order:
                samples.append(run_sample(kind, binaries[kind], case_args, "hot", False))

        expected_hash = equivalence["stdout_sha256"]
        for sample in samples:
            if sample["stdout_sha256"] != expected_hash or sample["exit_code"] != equivalence["exit_code"]:
                raise RuntimeError(f"timed output mismatch for {case_name}: {sample['binary']} {sample['cache']}")

        results[case_name] = {
            "args_count": len(case_args),
            "args_kind": "directory" if case_name == "full" else "files",
            "selection": case["selection"],
            "equivalence": equivalence,
            "samples": samples,
            "summary": summarize(samples),
        }

    meta = {
        "generated_at_unix_ns": time_ns(),
        "platform": platform.platform(),
        "python": sys.version.split()[0],
        "cpu_count": os.cpu_count(),
        "tblgen": str(tblgen),
        "tblgen_sha256": sha256_file(tblgen),
        "bench": str(bench),
        "bench_sha256": sha256_file(bench),
        "root": str(root),
        "tblgen_commit": git_value(args.repo, "rev-parse", "HEAD") if args.repo else None,
        "corpus_commit": git_value(args.corpus_repo, "rev-parse", "HEAD") if args.corpus_repo else None,
        "corpus_branch": git_value(args.corpus_repo, "rev-parse", "--abbrev-ref", "HEAD") if args.corpus_repo else None,
        "input_fingerprint": fingerprint,
        "selection_pool": selection_meta,
        "config": {
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
