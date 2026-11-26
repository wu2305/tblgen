#!/usr/bin/env python3
"""Generate deterministic typed-gen benchmark specs.

The files are inputs for benches/typed_gen.rs. They are synthetic JSON specs
(format: 1, nodes form) so every case exercises compile_generated; archive or
rawTable inputs would bypass the typed path.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path


def field(name: str, data_type: str = "C", dimension: bool = True, **extra) -> dict:
    out = {"name": name, "dataType": data_type, "isDimension": dimension}
    out.update(extra)
    return out


def base(nodes: list[dict]) -> dict:
    return {
        "format": 1,
        "version": "4.19.0",
        "properties": {"datasource": "defdw", "extractDataEnabled": False},
        "nodes": nodes,
    }


def file_input(key: str, fields: list[dict], alias: str | None = None, file_name: str = "in.xlsx#S") -> dict:
    node = {
        "key": key,
        "type": "FileInput",
        "alias": alias or key,
        "config": {"fileName": file_name},
        "fields": fields,
    }
    return node


def select(key: str, input_key: str, steps: list[dict] | None = None, fields: list[dict] | None = None) -> dict:
    node = {"key": key, "type": "Select", "inputs": [input_key]}
    if steps is not None:
        node["steps"] = steps
    if fields is not None:
        node["fields"] = fields
    return node


def output(input_key: str, key: str = "out") -> dict:
    return {"key": key, "type": "Output", "inputs": [input_key]}


def small() -> dict:
    src_fields = [
        field("dim", "C", True),
        field("measure", "N", False),
        field("note", "C", True),
    ]
    return base([
        file_input("src", src_fields, file_name="small.xlsx#S"),
        select(
            "flt",
            "src",
            steps=[
                {"type": "Filter", "fields": ["dim"], "filter": "[dim] != NULL"},
                {"type": "Refactor", "fields": ["measure"], "refactor": {"rename": "amount"}},
            ],
        ),
        output("flt"),
    ])


def wide(width: int = 1024) -> dict:
    fields = []
    for i in range(width):
        if i % 3 == 0:
            fields.append(field(f"dim_{i:04d}", "C", True))
        else:
            fields.append(field(f"measure_{i:04d}", "N", False))
    return base([
        {
            "key": "src",
            "type": "ModelTable",
            "alias": "wide_src",
            "config": {"moduleTablePath": "wide.tbl"},
            "fields": fields,
        },
        select("sel", "src"),
        output("sel"),
    ])


def deep(depth: int = 64, width: int = 8) -> dict:
    src_fields = [field(f"f{i}", "C", i % 2 == 0) for i in range(width)]
    nodes = [file_input("src", src_fields, file_name="deep.xlsx#S")]
    prev = "src"
    for i in range(depth):
        key = f"s{i:03d}"
        nodes.append(
            select(
                key,
                prev,
                steps=[{"type": "Filter", "fields": ["f0"], "filter": "[f0] != NULL"}],
            )
        )
        prev = key
    nodes.append(output(prev))
    return base(nodes)


def fan_in(inputs: int = 4, fields_per_input: int = 12) -> dict:
    nodes = []
    for i in range(inputs):
        key = f"a{i}"
        fields = [field("k", "C", True, dbfield=f"K{i}")]
        for j in range(fields_per_input):
            fields.append(field(f"v{i}_{j:02d}", "N", False))
        nodes.append(file_input(key, fields, alias=key, file_name=f"fan{i}.xlsx#S"))

    conditions = []
    for i in range(inputs - 1):
        left, right = f"a{i}", f"a{i + 1}"
        conditions.append(
            {
                "joinType": "InnerJoin",
                "leftTable": left,
                "rightTable": right,
                "clauses": [
                    {"leftExp": f"[{left}].[k]", "operator": "=", "rightExp": f"[{right}].[k]"}
                ],
            }
        )

    join_fields = [{"name": "k", "inputNode": "a0"}]
    for i in range(inputs):
        for j in range(fields_per_input):
            join_fields.append({"name": f"v{i}_{j:02d}"})

    nodes.append(
        {
            "key": "j",
            "type": "Join",
            "inputs": [f"a{i}" for i in range(inputs)],
            "config": {"joinConditions": conditions},
            "fields": join_fields,
        }
    )
    nodes.append(output("j"))
    return base(nodes)


def fan_out(branches: int = 8, width: int = 32) -> dict:
    src_fields = []
    for i in range(width):
        if i % 4 == 0:
            src_fields.append(field(f"dim_{i:02d}", "C", True))
        else:
            src_fields.append(field(f"measure_{i:02d}", "N", False))
    nodes = [file_input("src", src_fields, file_name="fanout.xlsx#S")]
    branch_keys = []
    for i in range(branches):
        key = f"b{i}"
        branch_keys.append(key)
        nodes.append(
            select(
                key,
                "src",
                steps=[{"type": "Filter", "fields": ["dim_00"], "filter": "[dim_00] != NULL"}],
            )
        )

    union_fields = [
        field(f["name"], f["dataType"], f["isDimension"])
        for f in src_fields
    ]
    nodes.append(
        {
            "key": "u",
            "type": "Union",
            "inputs": branch_keys,
            "config": {"unionType": "UnionAll"},
            "fields": union_fields,
        }
    )
    nodes.append(output("u"))
    return base(nodes)


def error_dense(raw_nodes: int = 24, steps_per_node: int = 8, fields: int = 8) -> dict:
    src_fields = [field(f"f{i}", "C", i % 2 == 0) for i in range(fields)]
    nodes = [file_input("src", src_fields, file_name="errors.xlsx#S")]
    select_fields = [field(f"f{i}", "C", i % 2 == 0) for i in range(fields)]
    for r in range(raw_nodes):
        key = f"bad_{r:03d}"
        steps = [
            {"type": f"NoSuchStep{r}_{s}", "fields": ["f0", "f1"]}
            for s in range(steps_per_node)
        ]
        nodes.append(
            {
                "key": key,
                "rawNode": {
                    "type": "Select",
                    "id": key,
                    "alias": key,
                    "inputNodes": ["src"],
                    "fields": select_fields,
                    "steps": steps,
                },
            }
        )
    nodes.append(output("src"))
    return base(nodes)


def select_heavy(chain: int = 16, width: int = 64) -> dict:
    """Long chain of Select nodes with rich typed steps.

    Amplifies the duplicate name-list evaluation G1 targets: every select
    runs ~10 typed steps (Filter/Refactor/RemoveField/AddField/
    TransformToCalculatedField), every fourth select plus the last one
    carries an explicit `fields` projection, and the last select ends in an
    opaque SplitField so the declared+opaque escape path is exercised.
    """
    dims = width // 2
    src_fields = [field(f"dim_{i:02d}", "C", True) for i in range(dims)]
    src_fields += [field(f"meas_{i:02d}", "N", False) for i in range(dims)]
    # name -> (dataType, isDimension); mirrors evolve_names for the step
    # types used below so explicit projections always match the derivation.
    meta = {f["name"]: (f["dataType"], f["isDimension"]) for f in src_fields}
    names = [f["name"] for f in src_fields]

    def find(pred: str) -> int | None:
        up = pred.upper()
        for i, n in enumerate(names):
            if n.upper() == up:
                return i
        return None

    def cur_pred(preds) -> str:
        for n in names:
            if all(n != p for p in preds) and meta[n][0] == "N":
                return n
        return names[0]

    def cur_dim() -> str:
        for n in names:
            if meta[n][1]:
                return n
        return names[-1]

    nodes = [file_input("src", src_fields, file_name="select-heavy.xlsx#S")]
    prev = "src"
    for c in range(chain):
        key = f"s{c:02d}"
        last = c == chain - 1
        explicit = (c % 4 == 2) or last
        steps: list[dict] = []

        # 1-2: filter + rename an N field
        f1 = cur_dim()
        steps.append({"type": "Filter", "fields": [f1], "filter": f"[{f1}] != NULL"})
        t1 = cur_pred([])
        r1 = f"rn_{c}_a"
        steps.append({"type": "Refactor", "fields": [t1], "refactor": {"rename": r1}})
        i = find(t1)
        assert i is not None
        names[i] = r1
        meta[r1] = meta.pop(t1)

        # 3: drop a dimension
        d1 = cur_dim()
        steps.append({"type": "RemoveField", "fields": [d1]})
        i = find(d1)
        assert i is not None
        names.pop(i)
        meta.pop(d1)

        # 4: computed N field referencing the renamed field
        af1 = f"calc_{c}_a"
        steps.append({
            "type": "AddField",
            "addField": {
                "name": af1,
                "dataType": "N",
                "isDimension": False,
                "exp": f"[{r1}] * 2",
            },
        })
        names.append(af1)
        meta[af1] = ("N", False)

        # 5: in-place transform of another N field (name-preserving)
        t2 = cur_pred([r1])
        steps.append({
            "type": "TransformToCalculatedField",
            "fields": [t2],
            "transformFieldInfo": {"dataType": "N", "exp": f"[{r1}] + 1"},
        })
        meta[t2] = ("N", meta[t2][1])

        # 6: rename a second N field
        t3 = cur_pred([r1, t2])
        r3 = f"rn_{c}_b"
        steps.append({"type": "Refactor", "fields": [t3], "refactor": {"rename": r3}})
        i = find(t3)
        assert i is not None
        names[i] = r3
        meta[r3] = meta.pop(t3)

        # 7: drop another dimension
        d2 = cur_dim()
        steps.append({"type": "RemoveField", "fields": [d2]})
        i = find(d2)
        assert i is not None
        names.pop(i)
        meta.pop(d2)

        # 8: constant computed dimension field
        af2 = f"calc_{c}_b"
        steps.append({
            "type": "AddField",
            "addField": {
                "name": af2,
                "dataType": "C",
                "length": 20,
                "isDimension": False,
                "exp": f"'{c}'",
            },
        })
        names.append(af2)
        meta[af2] = ("C", False)

        # 9: filter on the computed measure
        steps.append({"type": "Filter", "fields": [af1], "filter": f"[{af1}] > 0"})

        if last:
            # opaque reshaping as the final step: explicit fields are the
            # contract, no derivable comparison is possible
            steps.append({
                "type": "SplitField",
                "fields": [cur_dim()],
                "split": {"splitString": ",", "splitType": "Column"},
            })
        else:
            # 10: rename the constant field
            steps.append({
                "type": "Refactor",
                "fields": [af2],
                "refactor": {"rename": f"rc_{c}_b"},
            })
            i = find(af2)
            assert i is not None
            names[i] = f"rc_{c}_b"
            meta[f"rc_{c}_b"] = meta.pop(af2)

        node = select(key, prev, steps=steps)
        if explicit:
            node["fields"] = [
                {"name": n, "dataType": meta[n][0], "isDimension": meta[n][1]}
                for n in names
            ]
        nodes.append(node)
        prev = key
    nodes.append(output(prev))
    return base(nodes)


def select_rawstep() -> dict:
    """Minimal typed-spec rawStep coverage for the gen A/B.

    Two chained Selects exercise every rawStep branch on both validate and
    field_compute: typed prefix + mid rawStep + typed suffix with explicit
    fields (merged name check stops at rawStep; simulation stops with
    opaque_at; escape suffix recovery runs on borrowed steps), and a leading
    rawStep (validate early-return; escape walks the whole suffix). Shapes
    mirror the escape-path regressions in tests/dataflow.rs.
    """
    src_fields = [
        field("dim_0", "C", True),
        field("meas_0", "N", False),
        field("meas_1", "N", False),
        field("note", "C", True),
    ]
    s_mid = {
        "key": "s_raw_mid",
        "type": "Select",
        "inputs": ["src"],
        "fields": [
            {"name": "dim_0", "dataType": "C", "isDimension": True},
            {"name": "amount_0", "dataType": "N", "isDimension": False},
            {"name": "meas_1", "dataType": "N", "isDimension": False},
            {"name": "calc_y", "dataType": "N", "isDimension": False},
        ],
        "steps": [
            {"type": "Filter", "fields": ["dim_0"], "filter": "[dim_0] != NULL"},
            {"type": "Refactor", "fields": ["meas_0"], "refactor": {"rename": "amount_0"}},
            {"rawStep": {"type": "SplitField", "fields": ["note"]}},
            {
                "type": "AddField",
                "fields": ["calc_y"],
                "addField": {
                    "name": "calc_y",
                    "dataType": "N",
                    "isDimension": False,
                    "exp": "[amount_0]",
                },
            },
            {
                "type": "TransformToCalculatedField",
                "fields": ["calc_y"],
                "transformFieldInfo": {"exp": "[amount_0] * 2", "dataType": "N"},
            },
        ],
    }
    s_first = {
        "key": "s_raw_first",
        "type": "Select",
        "inputs": ["s_raw_mid"],
        "fields": [
            {"name": "dim_0", "dataType": "C", "isDimension": True},
            {"name": "amount_0", "dataType": "N", "isDimension": False},
            {"name": "measure_1", "dataType": "N", "isDimension": False},
            {"name": "calc_y", "dataType": "N", "isDimension": False},
            {"name": "total_x", "dataType": "N", "isDimension": False},
        ],
        "steps": [
            {"rawStep": {"type": "SplitField", "fields": ["meas_1"]}},
            {
                "type": "AddField",
                "fields": ["total_x"],
                "addField": {
                    "name": "total_x",
                    "dataType": "N",
                    "isDimension": False,
                    "exp": "[amount_0]",
                },
            },
            {"type": "Refactor", "fields": ["meas_1"], "refactor": {"rename": "measure_1"}},
        ],
    }
    return base([
        file_input("src", src_fields, file_name="rawstep.xlsx#S"),
        s_mid,
        s_first,
        output("s_raw_first"),
    ])


CASES = {
    "small": small,
    "wide": wide,
    "deep": deep,
    "fan-in": fan_in,
    "fan-out": fan_out,
    "error-dense": error_dense,
    "select-heavy": select_heavy,
    "select-rawstep": select_rawstep,
}


def main() -> int:
    out_dir = Path(sys.argv[1] if len(sys.argv) > 1 else "benches/fixtures/typed-gen")
    out_dir.mkdir(parents=True, exist_ok=True)
    for name, build in CASES.items():
        path = out_dir / f"{name}.spec.json"
        path.write_text(json.dumps(build(), indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
        print(path)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
