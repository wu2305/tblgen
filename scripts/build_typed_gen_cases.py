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


CASES = {
    "small": small,
    "wide": wide,
    "deep": deep,
    "fan-in": fan_in,
    "fan-out": fan_out,
    "error-dense": error_dense,
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
