---
name: tblgen
description: 从声明式 JSON 规格离线生成、校验和反解 BI 平台 DataFlow 加工模型（.tbl 文件）。用于新建或修改加工模型（库内加工、Excel/文件提取、引用已有模型、宽表转长表、行列转换、Join/Union/汇总等），把 SQL 加工意图转成 .tbl，或检查、读懂存量 .tbl。不连接数据库、不执行 SQL、不部署。
---

# tblgen：制作与检查 .tbl 加工模型

工作方式：写**规格**（spec，JSON）→ `check` → `gen` 出 `.tbl` → 再 `check` 产物。tblgen 只做离线的结构与引用校验，通过不代表平台上能执行；资源路径、数据源和 SQL 是否可用仍需用户在设计器里确认。

## 调用

启动器 `run.py` 位于本 `SKILL.md` 同目录。用绝对路径调用，路径加引号（可能含空格）；Windows 把 `python3` 换成 `py -3`。下文 `$RUN` 均指这个绝对路径：

```bash
python3 "$RUN" <命令> [参数]
```

| 命令 | 作用 |
| --- | --- |
| `schema` | 在 stdout 输出 `{"schema": ..., "examples": ...}`。不带任何参数。 |
| `check <文件>...` | 校验一个或多个规格 / `.tbl`，stdout 输出一份 `{"files":[{file, scope, errors, warnings}]}` 报告。 |
| `gen <spec.json> [-o <out.tbl>]` | 校验并生成 `.tbl`。不加 `-o` 时写 stdout。 |
| `decompile <in.tbl> [-o <out.json>]` | 把 `.tbl` 反解为规格，自动选择形态。 |
| `help`、`-h`、`--help` | 顶层帮助写 stderr；不要放在子命令之后。 |

- `-o` 只用于 `gen` 和 `decompile`，目标必须不存在。`schema` 不接受选项；`check` 即使收到 `-o 路径` 也不会写报告，要保存结果请捕获 stdout。没有 `--form`、`--force` 或 `--version`。
- stdout 只放 JSON 结果；下载进度、`tblgen: wrote ...`、帮助和 `gen` 的失败诊断都写 stderr（`gen` 每行一个 JSON 诊断）。两个流分开读。
- 退出码：`0` 成功（可能带 warnings），`1` 规格或模型无效，`2` 用法、I/O 或启动器错误。

## 新建模型流程

复制这份清单并逐项勾选：

```
- [ ] 1. 确认需求：输入源、输出字段及口径、最终 .tbl 放在哪个目录
- [ ] 2. 从 schema 示例中选一个起点，写进临时目录
- [ ] 3. 编辑规格
- [ ] 4. check 规格 → 按诊断修改 → 重复，直到 errors 为空
- [ ] 5. gen 到新路径，再 check 产物 .tbl
- [ ] 6. 写入用户指定的位置，并按「交付」一节汇报
```

### 第 2 步：选起点

`schema` 输出的 `schema` 键是规格本身的 JSON Schema（Draft 2020-12），不是 `.tbl` 格式，也不是数据库 schema。`examples` 下的每一项都是可直接 `gen` 的完整规格：

| 路径 | 内容 |
| --- | --- |
| `examples.typed[i]` | 通用加工链，本身就是规格。 |
| `examples.nodeExamples[i].spec` | 20 种节点，按 `type` 选（如 `ColToRow`、`Join`、`Sql`）。包含该节点需要的上下游。 |
| `examples.stepExamples[i].spec` | 24 种 Select 步骤，按 `type` 选（如 `Filter`、`Refactor`），看其中 Select 节点的 `steps`。 |
| `examples.rawOnly[i]` | `rawNode`/`rawStep` 透传，以及 `archive`、`rawTable` 快照形态。 |

`label: "typed"` 表示支持类型化配置，不代表能推导全部字段；`"rawOnly"` 只能原样透传，不能证明业务语义正确。`schema.oneOf` 区分 `nodes`、`archive`、`rawTable` 三种规格；`schema.$defs` 给出字段和载荷结构。不要将整个 `schema` 输出作为 `gen` 的输入。

取出示例时用 Python 写文件（PowerShell 的 `>` 会写成 UTF-16）：

```python
import json, subprocess, sys, tempfile
from pathlib import Path
RUN = "/abs/path/to/tblgen/run.py"
doc = json.loads(subprocess.check_output([sys.executable, RUN, "schema"]))
spec = next(e["spec"] for e in doc["examples"]["nodeExamples"]
            if e["type"] == "ColToRow" and e["label"] == "typed")
out = Path(tempfile.mkdtemp()) / "model.spec.json"
out.write_text(json.dumps(spec, ensure_ascii=False, indent=2), encoding="utf-8")
print(out)
```

更贴近业务的完整示例在 tblgen 仓库的 `examples/` 目录下：`budget.spec.json` 是 Excel 提取，`property-model-long.spec.json` 是引用已有模型并宽转长。

### 第 3 步：编写规格

- 顶层是 `format: 1`、`properties`、`nodes`。每个节点有唯一的 `key`，`inputs` 只引用上游的 `key`，不填别名或 `.tbl` 路径。
- 外部输入节点（`ModelTable`、`DbTable`、`FileInput` 等）要显式声明每个字段的 `name`、`dataType`、`isDimension`，按源结构补上 `dbfield`、`length`、`decimal`。字段不清楚就问用户，不要猜。
- `ModelTable.config.moduleTablePath` 是相对**最终 `.tbl` 所在目录**的路径（如 `../ods/ods_demo.tbl`），不是相对临时规格文件。
- 简单的 Select/Output 字段可以由工具推导。Join、Union、行列转换的结果字段要显式写出。不要手工维护 `fields/dimensions/measures` 的重复副本。
- Select 的 `steps` 按顺序执行，例如：
  - 过滤：`{"type":"Filter","fields":["金额"],"filter":"[金额] > 0"}`
  - 改名：`{"type":"Refactor","fields":["金额"],"refactor":{"rename":"预算额"}}`

**SQL 需求怎么落**：过滤可用 `ModelTable → Select → Output`，关联、分组分别优先考虑 `Join`、`Agg`。`[金额] > 0` 是平台 Filter 表达式，不带 `WHERE`。若必须保留原生查询，如 `select amount from budget where amount > 0`，从 `Sql` 节点示例起步，在 `config.sql`（不是 `config.selectSql`）写查询，显式声明返回字段；物理表名、列名不能由中文名猜测。`gen` 不输出最终 SQL，`check` 不连接数据库。默认只写单条只读查询；多语句或写操作须先说明副作用并单独确认。

### 第 4–5 步：校验与生成

```bash
python3 "$RUN" check model.spec.json
python3 "$RUN" gen model.spec.json -o model.tbl
python3 "$RUN" check model.tbl
```

任一步退出码不为 0 就停下来修复。修复时按每条诊断的 `path`（JSON Pointer，如 `/nodes/0/inputs/0`）和 `rule`（如 `graph.danglingInput`）回到规格里改。读报告时看 `scope`：`spec`、`dataflow` 表示确实校验过；`skipped` 表示文件不是 DataFlow，**没有校验**，不能算通过。

## 修改或读懂存量模型

```bash
python3 "$RUN" decompile existing.tbl -o existing.spec.json
```

反解结果的顶层键说明了形态：

- `nodes`：可编辑的生成形态，但其中仍可能有 raw 节点。
- `archive`：DataFlow 保真快照，改字段后**不会**重新推导下游。
- `rawTable`：非 DataFlow 文件的原样快照。

只做小改动时，可以在反解结果上编辑，再 `gen` 到新路径。如果改动需要重新推导字段，就按来源重建 typed 节点图，不要把 raw 往返当作语义验证。

## 交付

只写入用户指定的位置。汇报内容：

- 规格和 `.tbl` 的路径
- 最后一次 `check` 的实际结果（scope、warnings）
- 哪些字段和口径已经由用户确认、哪些是推断
- 还需要用户在平台上核对的外部依赖：引用的模型或数据源路径、原生 SQL、设计器里保存和预览

## 启动器问题排查

- 首次运行会从 CNB Release 下载当前平台的二进制并校验 SHA-256，之后使用缓存。支持 macOS-arm64、Linux-amd64、Windows-amd64。
- 无法联网时，设置 `TBLGEN_BIN=<本地二进制>`。这个文件必须与发布产物的 SHA-256 一致，否则会被拒绝执行。`TBLGEN_CACHE_DIR` 可以改缓存目录。
- `tblgen launcher: ...` 开头、退出码为 2 的错误来自启动器（网络、哈希不符、缺少 `release.json`），不是规格问题，不要去改规格。
