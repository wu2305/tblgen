"""Measure frozen leave-one-out builds, retaining raw timings and diagnostic deltas."""

import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import time

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
from build_check_ablations import GROUPS, LABELS, OPTIMIZATIONS, ROOT


def controls(directory):
    fields = [{"name": "A", "dataType": "C", "isDimension": True}]
    doc = {"version": "4.19.0", "properties": {"modelDataType": "DataFlow", "extractDataEnabled": False},
           "dimensions": fields, "dataFlow": {"nodes": {
               "a": {"type": "FileInput", "alias": "a", "id": "a", "fileName": "x.xlsx#S", "fields": fields},
               "s": {"type": "Select", "alias": "s", "id": "s", "inputNodes": ["a"], "fields": fields},
               "default": {"type": "Output", "alias": "out", "id": "default", "inputNodes": ["s"], "fields": fields},
           }}}
    def change(group):
        # Decode independent JSON values: no aliases between input/output fields.
        v = json.loads(json.dumps(doc))
        n = v["dataFlow"]["nodes"]
        if group == "fields":
            n["a"]["originalFields"] = [{"name": "Broken"}]
        elif group == "graph":
            n["s"]["inputNodes"] = ["ghost"]
        elif group == "node_rules":
            n["default"]["inputNodes"] = []
        elif group == "select":
            n["s"]["steps"] = [{"type": "AddField", "addField": {
                "name": "x", "exp": "'x'", "dataType": "C", "isDimension": True}}]
        elif group == "lineage":
            n["s"]["fields"][0]["inputField"] = "ghost"
        elif group == "partition":
            v["dimensions"] = []
        elif group == "extract":
            v["properties"].update(extractDataEnabled=True, dbTableName="ablation_output")
        elif group == "references":
            n["a"].update(type="ModelTable", moduleTablePath="./missing.tbl")
            del n["a"]["fileName"]
        return v

    folder = directory / "Example" / "data" / "tables"
    folder.mkdir(parents=True)
    paths = []
    for label in ("clean", *GROUPS):
        path = folder / (label + ".tbl")
        path.write_text(json.dumps(change(label)))
        paths.append(str(path))
    return paths


EXPECTED = {
    "fields": ("field.dataType", "/dataFlow/nodes/a/originalFields/0/dataType"),
    "graph": ("graph.danglingInput", "/dataFlow/nodes/s/inputNodes"),
    "node_rules": ("err.dataflow.node.input", "/dataFlow/nodes/default/inputNodes"),
    "select": ("structure.step.fields", "/dataFlow/nodes/s/steps/0/fields"),
    "lineage": ("field.inputField.missing", "/dataFlow/nodes/s/fields/0/inputField"),
    "partition": ("flow.output.partition", "/dataFlow/nodes/default/fields/0/name"),
    "extract": ("warn.extract.dbfield", "/dimensions/0/dbfield"),
    "references": ("ref.modelTable.missing", "/dataFlow/nodes/a/moduleTablePath"),
}


def cli(exe, paths):
    t = time.perf_counter_ns()
    p = subprocess.run([exe, "check", *paths], capture_output=True)
    elapsed = time.perf_counter_ns() - t
    if p.returncode not in (0, 1):
        raise RuntimeError(f"{exe}: exit {p.returncode}: {p.stderr[:500]!r}")
    return p.stdout, [p.returncode, hashlib.sha256(p.stdout).hexdigest()], elapsed


def reports(exe, paths):
    output, signature, _ = cli(exe, paths)
    rows = json.loads(output)["files"]
    assert [r["file"] for r in rows] == paths
    assert all(r["scope"] == "dataflow" for r in rows)
    return rows, signature


def delta(before, after):
    lost = Counter()
    newly_clean = changed = 0
    for a, b in zip(before, after, strict=True):
        previous = sum(lost.values())
        newly_clean += bool(a["errors"]) and not b["errors"]
        for kind in ("errors", "warnings"):
            # Must be an ordered subsequence, with exact messages, pointers and multiplicity.
            j = 0
            for d in a[kind]:
                if j < len(b[kind]) and d == b[kind][j]:
                    j += 1
                else:
                    lost[d["rule"]] += 1
            if j != len(b[kind]):
                raise ValueError(f"new or reordered diagnostics in {b['file']}")
        changed += sum(lost.values()) != previous
    warnings = sum(v for k, v in lost.items() if k.startswith("warn."))
    return {"lost_errors": sum(lost.values()) - warnings, "lost_warnings": warnings,
            "files_affected": changed, "error_to_no_error": newly_clean, "lost_rules": dict(lost)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("builds", type=Path)
    parser.add_argument("manifest", type=Path)
    parser.add_argument("experiment", type=Path)
    args = parser.parse_args()
    builds = args.builds.resolve()
    paths = json.loads(args.manifest.read_bytes())
    assert paths and len(paths) == len(set(paths))
    digest = hashlib.sha256()
    for p in paths:
        digest.update(hashlib.sha256(Path(p).read_bytes()).digest())
    out = {"build": json.loads((builds / "build.json").read_bytes()),
           "corpus_sha256": digest.hexdigest(), "documents": len(paths),
           "experiment_sha256": hashlib.sha256(args.experiment.read_bytes()).hexdigest(),
           "deltas": {}, "control_hits": {}, "historical": {}}
    old = [str(ROOT / "tests/fixtures/ads_owner_repair_history.tbl"),
           *map(str, sorted((ROOT / "tests/fixtures/cx").glob("*.tbl")))]
    sigs = {}
    with tempfile.TemporaryDirectory() as tmp:
        probes = controls(Path(tmp))
        for label in LABELS:
            exe = builds / label / "tblgen"
            rows, sigs[label] = reports(exe, paths)
            test, _ = reports(exe, probes)
            history, _ = reports(exe, old)
            assert test[0]["errors"] == test[0]["warnings"] == []
            hits = {}
            for group, row in zip(GROUPS, test[1:], strict=True):
                hits[group] = EXPECTED[group] in [(d["rule"], d["path"]) for d in row["errors"] + row["warnings"]]
                if hits[group] != (label != group):
                    raise ValueError(f"unexpected control {label}/{group}: {row}")
            if label == "full":
                base, base_history = rows, history
                out["baseline"] = {"errors": sum(len(r["errors"]) for r in base),
                                   "warnings": sum(len(r["warnings"]) for r in base),
                                   "error_files": sum(bool(r["errors"]) for r in base),
                                   "cli_signature": sigs[label]}
            out["control_hits"][label] = hits
            out["deltas"][label] = delta(base, rows)
            out["historical"][label] = {Path(a["file"]).name: delta([a], [b])["lost_rules"]
                                        for a, b in zip(base_history, history, strict=True)}
            if label in OPTIMIZATIONS:
                assert sigs[label] == sigs["full"] and not any(out["historical"][label].values())
    n = len(LABELS)
    offsets = [0] + [(j+1)//2 if j % 2 else -(j//2) for j in range(1, n)]
    orders = [[LABELS[(i+off) % n] for off in offsets] for i in range(n)]
    out["orders"] = orders
    cases = {"corpus_wire": ("manifest:" + str(args.manifest.resolve()), 1),
             "experiment_wire": (str(args.experiment.resolve()), 150),
             "width256_wire": ("width:256", 100), "corpus_cli": (None, 1)}
    out["timings"] = {}
    for name, (source, repeats) in cases.items():
        times = {label: [] for label in LABELS}
        for idx, order in enumerate([list(LABELS), *orders]):
            for label in order:
                if source is None:
                    _, sig, elapsed = cli(builds / label / "tblgen", paths)
                    assert sig == sigs[label]
                else:
                    result = json.loads(subprocess.check_output(
                        [builds / label / "wire_check", "time", source, str(repeats)]))
                    assert result["documents"] == (len(paths) if name == "corpus_wire" else 1)
                    assert result["repeats"] == repeats
                    elapsed = result["elapsed_ns"]
                if idx:
                    times[label].append(elapsed)
        out["timings"][name] = {"repeats": repeats, "batch_ns": times}
        print(f"finished {name}", file=sys.stderr, flush=True)
    json.dump(out, sys.stdout, indent=2)
    print()


if __name__ == "__main__":
    main()
