"""Build experiment-only leave-one-out binaries in a NEW directory outside the repo."""

import argparse
import difflib
import hashlib
import json
from pathlib import Path
import shutil
import subprocess


ROOT = Path(__file__).resolve().parents[1]
GROUPS = ("fields", "graph", "node_rules", "select", "lineage", "partition", "extract", "references")
OPTIMIZATIONS = ("no_lineage_cache", "linear_partition", "eager_paths")
LABELS = ("full", *GROUPS, *OPTIMIZATIONS)
CHECK, LINEAGE, OUTPUT = "src/check.rs", "src/check/lineage.rs", "src/check/output.rs"


def substitute(text, old, new):
    if text.count(old) != 1:
        raise ValueError(f"expected exactly one source anchor: {old}")
    return text.replace(old, new)


def omit(text, begin, end):
    if text.count(begin) != 1 or text.count(end) != 1:
        raise ValueError(f"ambiguous source range: {begin} .. {end}")
    start = text.index(begin)
    return text[:start] + text[text.index(end, start) + len(end):]


def variant(sources, label):
    check, lineage, output = (sources[p] for p in (CHECK, LINEAGE, OUTPUT))
    if label == "fields":
        for key, filtered in (("fields", "true"), ("originalFields", "false")):
            check = substitute(check, f'        check_fields(&mut diags, node, &base, "{key}", {filtered});\n', "")
    elif label == "graph":
        check = omit(check, "    // dangling inputs\n", "    check_acyclic(&mut diags, &infos, &order);\n")
    elif label == "node_rules":
        check = substitute(check, "        check_node_condition_keys(&mut diags, node, &base);\n", "")
        check = substitute(check, "    check_arity_and_type_rules(&mut diags, tbl, nodes_obj, &infos, &order);\n", "")
    elif label == "select":
        check = omit(check, "    let param_names_w:", "    check_select_steps(&mut diags, nodes_obj, &infos, &order, &param_names_w);\n")
    elif label == "lineage":
        check = substitute(check, "    lineage::check_lineage(&mut diags, nodes_obj);\n", "")
    elif label == "partition":
        check = substitute(check, "    output::check_output_partition(diags, tbl, nodes_obj);\n", "")
    elif label == "extract":
        check = substitute(check, "    output::check_extract_dbfields(diags, tbl);\n", "")
    elif label == "references":
        check = omit(check, "        if let Some(at) = crate::project::locate(path) {\n", "            d.extend(check_references(&v, &at));\n        }\n")
    elif label == "no_lineage_cache":
        lineage = substitute(lineage, "    let mut names_by_pred: HashMap<&str, HashSet<String>> = HashMap::new();\n", "")
        lineage = substitute(lineage, "names_by_pred\n                .entry(key)\n                .or_insert_with(|| names_of(src_node))", "names_of(src_node)")
    elif label == "linear_partition":
        output = substitute(output, "fn partition_names<'a>(v: &'a Value, out: &mut Vec<&'a str>)", "fn partition_names(v: &Value, out: &mut Vec<String>)")
        output = substitute(output, "out.push(n);", "out.push(n.to_string());")
        output = omit(output, "#[derive(Default)]\nstruct PartitionNames", "\n}\n\n/// The default Output")
        output = substitute(output, " node's live field names", "/// The default Output node's live field names")
        output = output.replace("PartitionNames::default()", "Vec::new()").replace("dim_names.names", "dim_names").replace("meas_names.names", "meas_names")
        output = substitute(output, "!top.contains(nm)", "!top.iter().any(|t| crate::model::field_eq(t, nm))")
    elif label == "eager_paths":
        anchor = "    let Some(v) = node.get(key) else { return };\n"
        check = substitute(check, anchor, anchor + '    let path = format!("{base}/{key}");\n')
        check = substitute(check, '            format!("{base}/{key}"),\n', "            path,\n")
        anchor = "    for (i, f) in arr.iter().enumerate() {\n        if !f.is_object() {"
        check = substitute(check, anchor, anchor.replace("        if", '        let fpath = format!("{path}/{i}");\n        if'))
        check = substitute(check, 'format!("{base}/{key}/{i}")', "fpath")
        for tail in ("name", "dataType"):
            check = substitute(check, 'format!("{base}/{key}/{i}/' + tail + '")', 'format!("{fpath}/' + tail + '")')
    elif label != "full":
        raise ValueError(label)
    return dict(zip((CHECK, LINEAGE, OUTPUT), (check, lineage, output)))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("dest", type=Path)
    dest = parser.parse_args().dest.resolve()
    if dest == ROOT or ROOT in dest.parents:
        parser.error("destination must be a new directory outside the checkout")
    dest.mkdir(exist_ok=False)
    tree = dest / "snapshot"
    tree.mkdir()
    for name in ("src", "benches"):
        shutil.copytree(ROOT / name, tree / name)
    for name in ("Cargo.toml", "Cargo.lock"):
        shutil.copy2(ROOT / name, tree / name)
    originals = {f: (tree / f).read_text() for f in (CHECK, LINEAGE, OUTPUT)}
    metadata = {
        "rustc": subprocess.check_output(["rustc", "-Vv"], text=True),
        "sources": {str(p.relative_to(tree)): hashlib.sha256(p.read_bytes()).hexdigest()
                    for p in [*sorted((tree / "src").rglob("*.rs")), tree / "Cargo.toml", tree / "Cargo.lock", tree / "benches/wire_check.rs"]},
    }
    (dest / "build.json").write_text(json.dumps(metadata, indent=2))
    for label in LABELS:
        folder = dest / label
        folder.mkdir()
        patch = []
        for f, text in variant(originals, label).items():
            if (tree / f).read_text() != text:
                (tree / f).write_text(text)
            patch.extend(difflib.unified_diff(originals[f].splitlines(True), text.splitlines(True), f, f))
        (folder / "variant.diff").write_text("".join(patch))
        proc = subprocess.run(["cargo", "build", "--locked", "--release", "--bin", "tblgen",
                               "--bench", "wire_check", "--message-format=json"], cwd=tree, check=True, capture_output=True)
        built = set()
        for line in proc.stdout.splitlines():
            record = json.loads(line)
            if record["reason"] == "compiler-artifact" and record.get("executable"):
                name = record["target"]["name"]
                if name in ("tblgen", "wire_check"):
                    shutil.copy2(record["executable"], folder / name)
                    built.add(name)
        if built != {"wire_check", "tblgen"}:
            raise ValueError(f"missing executable for {label}")
        print(label, flush=True)
    for f, text in originals.items():
        (tree / f).write_text(text)


if __name__ == "__main__":
    main()
