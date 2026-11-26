"""Generate deterministic Select-step benchmark wires into an empty output directory."""

import argparse
import hashlib
import json
from pathlib import Path


def field(name, input_field=None):
    item = {"name": name, "dataType": "C", "isDimension": True}
    if input_field is not None:
        item["inputField"] = input_field
    return item


def evolve(names, step):
    ty = step["type"]
    if ty == "RemoveField":
        operands = {x.upper() for x in step["fields"]}
        return [n for n in names if n.upper() not in operands]
    if ty == "Refactor":
        old = step["fields"][0]
        new = step["refactor"]["rename"]
        out = list(names)
        for i, name in enumerate(out):
            if name.upper() == old.upper():
                out[i] = new
                break
        return out
    if ty == "AddField":
        return names + [step["addField"]["name"]]
    return names


def make(path, width, count, pattern):
    names = [f"F{i}" for i in range(width)]
    current = list(names)
    steps = []
    for i in range(count):
        if pattern == "filter":
            steps.append({
                "type": "Filter",
                "fields": [current[i % len(current)]],
                "filter": f"[{current[i % len(current)]}] != NULL AND [{current[(i + width // 2) % len(current)]}] != ''",
            })
        elif pattern == "rename":
            target = current[0]
            new = f"R{i}_{target}" if i % 2 == 0 else "F0"
            steps.append({
                "type": "Refactor",
                "fields": [target],
                "originFieldInfo": {"name": target, "dataType": "C"},
                "refactor": {"rename": new},
            })
        elif pattern == "removeadd":
            target = "F0"
            if i % 2 == 0:
                steps.append({"type": "RemoveField", "fields": [target]})
            else:
                steps.append({
                    "type": "AddField",
                    "fields": [target],
                    "addField": {
                        "name": target,
                        "dataType": "C",
                        "isDimension": True,
                        "length": 20,
                        "exp": "[F1]",
                    },
                })
        elif pattern == "uniqueexpr":
            steps.append({
                "type": "Filter",
                "fields": [current[i % len(current)]],
                "filter": f"[{current[i % len(current)]}] != NULL /* {i} */",
            })
        elif pattern == "missingexpr":
            steps.append({
                "type": "Filter",
                "fields": [current[i % len(current)]],
                "filter": f"[missing_{i}] != NULL",
            })
        else:
            raise ValueError(pattern)
        current = evolve(current, steps[-1])

    initial = [field(n) for n in names]
    final = [field(n, n) for n in current]
    doc = {
        "version": "4.19.0",
        "properties": {"modelDataType": "DataFlow", "extractDataEnabled": False},
        "dimensions": final,
        "dataFlow": {"nodes": {
            "src": {
                "type": "ModelTable",
                "id": "src",
                "alias": "src",
                "fields": initial,
                "moduleTablePath": "m.tbl",
            },
            "sel": {
                "type": "Select",
                "id": "sel",
                "alias": "sel",
                "inputNodes": ["src"],
                "fields": final,
                "steps": steps,
            },
            "default": {
                "type": "Output",
                "id": "default",
                "alias": "out",
                "inputNodes": ["sel"],
                "fields": final,
            },
        }},
    }
    text = json.dumps(doc, ensure_ascii=False, separators=(",", ":"))
    path.write_text(text)
    return hashlib.sha256(text.encode()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path, help="existing empty directory")
    args = parser.parse_args()
    if any(args.output.iterdir()):
        parser.error("output directory must be empty")
    cases = [
        (1, 1, "filter"),
        (8, 1, "filter"),
        (8, 4, "rename"),
        (256, 128, "filter"),
        (1024, 64, "filter"),
        (256, 128, "rename"),
        (256, 128, "removeadd"),
        (256, 128, "uniqueexpr"),
        (256, 128, "missingexpr"),
    ]
    manifest = {}
    for width, count, pattern in cases:
        name = f"{pattern}-{width}x{count}.tbl"
        manifest[name] = {"sha256": make(args.output / name, width, count, pattern)}
    print(json.dumps(manifest, indent=2))


if __name__ == "__main__":
    main()
