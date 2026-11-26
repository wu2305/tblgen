#!/usr/bin/env python3
"""Attach SHA-256 input fingerprints and baseline metadata to typed_gen bench JSON."""

import hashlib
import json
import pathlib
import sys

REPO = pathlib.Path(__file__).resolve().parents[1]
PERFORMANCE_BASELINE = "fd1be96a3145c502052e7625616615ce22aa02ba"


def sha256_file(path: pathlib.Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main() -> None:
    if len(sys.argv) != 3:
        raise SystemExit("usage: finalize_typed_gen_g0.py <bench.json> <out.json>")
    src, dst = map(pathlib.Path, sys.argv[1:])
    data = json.loads(src.read_text())
    for case in data["cases"]:
        name = case["name"]
        rel = (
            "tests/fixtures/experiment.spec.json"
            if name == "experiment"
            else f"benches/fixtures/typed-gen/{name}.spec.json"
        )
        case["input_path"] = rel
        case["input_sha256"] = sha256_file(REPO / rel)
    data["baseline"] = {
        "performance_baseline_commit": PERFORMANCE_BASELINE,
        "performance_baseline_branch": "auto/select-step-index-d521",
        "cargo_lock_sha256": sha256_file(REPO / "Cargo.lock"),
        "rustc": "rustc 1.99.1 (58cf8f9c3 2026-10-13)",
        "host": "x86_64-unknown-linux-gnu",
        "cargo_profile": "bench (optimized)",
    }
    data["equivalence"] = {
        "profile_entry_matches_generate_with_diagnostics": all(
            case["equivalence"]["profile_matches_normal"] for case in data["cases"]
        ),
        "note": "bench exits nonzero before writing if any case diverges",
    }
    dst.write_text(json.dumps(data, indent=2, ensure_ascii=False) + "\n")


if __name__ == "__main__":
    main()
