---
name: tblgen
description: 离线生成、校验和反解 SuccBI 平台 DataFlow 加工模型（.tbl 文件）及其服务端脚本（.action.ts）。用于新建或修改加工模型（脚本型、库内加工、Excel/文件提取、引用已有模型、宽表转长表、行列转换、Join/Union/汇总等），把 SQL 加工意图转成 .tbl，检查整个项目的模型引用与脚本规则，或读懂存量 .tbl。不连接数据库、不执行 SQL、不部署。
---

# tblgen：制作与检查 .tbl 加工模型

工作方式：用 `new` 或写**规格**（spec，JSON）→ `check` → `gen` 出 `.tbl` → 再 `check` 产物。tblgen 只做离线的结构与引用校验，通过不代表平台上能执行；资源路径、数据源和 SQL 是否可用仍需用户在设计器里确认。

## 调用

启动器 `run.py` 位于本 `SKILL.md` 同目录。用绝对路径调用，路径加引号（可能含空格）；Windows 把 `python3` 换成 `py -3`。下文 `$RUN` 均指这个绝对路径：

```bash
python3 "$RUN" <命令> [参数]
```

| 命令 | 作用 |
| --- | --- |
| `schema` | 在 stdout 输出 `{"schema": ..., "examples": ...}`。不带任何参数。 |
| `check <文件或目录>...` | 校验规格、`.tbl` 和 `.action.ts`；目录会递归展开。stdout 输出一份 `{"files":[{file, scope, errors, warnings}]}` 报告。 |
| `new script\|dsl ...` | 直接生成常见模型（见「用 new 起步」），写 `.tbl`，脚本型还写脚本骨架。 |
| `gen <spec.json> [-o <out.tbl>]` | 校验并生成 `.tbl`。不加 `-o` 时写 stdout。 |
| `decompile <in.tbl> [-o <out.json>]` | 把 `.tbl` 反解为规格，自动选择形态。 |
| `help`、`-h`、`--help` | 完整用法写 stderr；`new --help` 同样可用。 |

- 所有写文件的命令都不覆盖已有文件，目标必须不存在。`schema` 不接受选项；`check` 不写文件，要保存报告请捕获 stdout。完整参数见 `"$RUN" --help`。
- stdout 只放 JSON 结果；下载进度、`tblgen: wrote ...`、帮助和 `gen`/`new` 的诊断都写 stderr（每行一个 JSON 诊断）。两个流分开读。`rule` 以 `warn.` 开头的是警告，不会让命令失败，但要读并在交付时说明。
- 退出码：`0` 成功（可能带 warnings），`1` 规格或模型无效，`2` 用法、I/O 或启动器错误。

## 先定运行方式：提取还是查询

`properties.extractDataEnabled` 决定模型怎么运行，是设计决定，不是可有可无的开关：

- `true`（ETL）：运行时把结果写进 `dbTableName` 这张物理表，查询读这张表，调度按本模型排程。必须同时给出 `dbTableName`，否则平台按「不提取」处理（`warn.properties.runMode.noTable`）。
- `false`（查询逻辑）：不落表，查询时把节点图编译成 SQL 执行；调度跳过本模型，改为依赖它的上游。
- 不写：平台按 `false` 加载。`gen`/`check` 会给出 `warn.properties.runMode.unset` 提醒。

用户没说明时按用途问清楚：要给报表或下游直接读一张表的，用 `true`；只是把几张模型组合成一个可查询视图的，用 `false`。

## 新建模型流程

复制这份清单并逐项勾选：

```
- [ ] 1. 确认需求：输入源、输出字段及口径、运行方式、最终 .tbl 放在哪个目录
- [ ] 2. 选起点：常见形态用 `new`（见下），直接得到 .tbl，跳到第 5 步；其他从 schema 示例中选一个，写进临时目录
- [ ] 3. 编辑规格
- [ ] 4. check 规格 → 按诊断修改 → 重复，直到 errors 为空
- [ ] 5. gen 到新路径（用 new 的已经生成），再 check 产物 .tbl，放进项目后 check 整个项目
- [ ] 6. 写入用户指定的位置，并按「交付」一节汇报
```

### 第 2 步：用 new 起步

两种常见形态可以一条命令生成。`--extract` 必填，`-o` 是最终位置（目录不存在会创建）：

```bash
# 脚本型：Script 节点 → 输出；脚本骨架写到 <项目>/data/script/<md5(表名)>.action.ts
python3 "$RUN" new script --table ods_demo --schema ods --extract true \
  --fields "stat_month:C:7,corp_code:C:20,amount:N:18.2:m" -o <项目>/data/tables/ods/ods_demo.tbl

# 声明式：引用已有模型 →（Join）→ 列加工 → 输出；字段只写名字，定义取自上游
python3 "$RUN" new dsl --table dwd_demo --schema dwd --extract true \
  --input ../ods/ods_demo.tbl --input '$DATA:/dim/dim_corp.tbl' --join-on corp_code \
  --fields "stat_month,corp_code,amount" -o <项目>/data/tables/dwd/dwd_demo.tbl
```

- 字段串：`name:TYPE[:length[.decimal]][:m]`，TYPE 为 `C/N/I/D/P`，`m` 表示度量；`dbfield` 取大写名。
- `--input` 按模型里的写法给（相对路径或 `$DATA:/...`），按平台规则解析后读取上游字段；单输入可省略 `--fields`，取上游全部字段。
- 脚本骨架默认是占位版（预览返回字段结构，正式运行报错）；有上游输入的转换加 `--template transform`。不想写脚本用 `--no-script`。`--cn`、`--note` 只写进脚本头注释；模型的中文名不在 `.tbl` 里，按仓库约定维护（通常是目录 `.meta` 的 `desc`）。只属于另一种模型的参数会直接报错。
- 加 `--spec-out <路径>` 可同时保存规格，以后在它上面继续加步骤再 `gen`。
- 命令只填平台通用的默认值（数据源 `defdw`、设计器新建模型的表属性）。分层、表名前缀、数据源等仓库约定按仓库文档传参。

### 选其他起点

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

更贴近业务的完整示例在本技能包的 `examples/` 目录（与 `run.py` 同级）：

- `examples/budget.spec.json`：Excel 提取 → 宽转长（`FileInput → Select → ColToRow → Select → Output`）。
- `examples/property-model-long.spec.json`：引用已有模型 → 宽转长（`ModelTable → Select → ColToRow → Select → Output`）。

需求和其中之一相近时，复制到临时目录再改，比从单节点示例拼装更可靠。

### 第 3 步：编写规格

- 顶层是 `format: 1`、`properties`、`nodes`。每个节点有唯一的 `key`，`inputs` 只引用上游的 `key`，不填别名或 `.tbl` 路径。
- `properties` 的类型化键就是平台加载器读取的模型属性（`schema` 输出的 `schema.oneOf[0].properties.properties.properties` 列出全部，如 `extractDataEnabled`、`rollbackEnabled`、`modifyDbTableEnabled`、`primaryKeys`），会做类型检查。其他属性（如 `desc`）放进 `properties.raw`，原样合并到 `.tbl`。类型化键同时出现在 `raw` 里也可以，只要值相同；不同会报 `structure.properties.raw.conflict`。
- 外部输入节点（`ModelTable`、`DbTable`、`FileInput` 等）要显式声明每个字段的 `name`、`dataType`、`isDimension`，按源结构补上 `dbfield`、`length`、`decimal`。字段不清楚就问用户，不要猜。
- `ModelTable.config.moduleTablePath` 是相对**最终 `.tbl` 所在目录**的路径（如 `../ods/ods_demo.tbl`），不是相对临时规格文件。单独 check 规格或临时目录里的 `.tbl` 时查不到这个引用，放进项目后 check 项目才会核对它是否存在。
- 简单的 Select/Output 字段可以由工具推导。Join、Union、行列转换的结果字段要显式写出。不要手工维护 `fields/dimensions/measures` 的重复副本。
- Select 的 `steps` 按顺序执行，例如：
  - 过滤：`{"type":"Filter","fields":["金额"],"filter":"[金额] > 0"}`
  - 改名：`{"type":"Refactor","fields":["金额"],"refactor":{"rename":"预算额"}}`
- Join 的 `joinType` 取 `LeftJoin`、`InnerJoin`、`RightJoin`、`FullJoin`、`CrossJoin`、`AntiJoin`、`SemiJoin` 之一，写错会在保存时丢失。
- 表达式（`filter`、`exp` 等）用平台语法：字段写 `[字段名]`，字符串用单引号，如 `[版本] = '最终版'`、`[月份] >= '2025-01'`。`check` 会报出引用了不存在字段的表达式，但不检查语法（括号、引号是否配对），要自己核对。

**SQL 需求怎么落**：过滤可用 `ModelTable → Select → Output`，关联、分组分别优先考虑 `Join`、`Agg`。`[金额] > 0` 是平台 Filter 表达式，不带 `WHERE`。若必须保留原生查询，如 `select amount from budget where amount > 0`，从 `Sql` 节点示例起步，在 `config.sql`（不是 `config.selectSql`）写查询，显式声明返回字段；物理表名、列名不能由中文名猜测。`gen` 不输出最终 SQL，`check` 不连接数据库。默认只写单条只读查询；多语句或写操作须先说明副作用并单独确认。

### 第 4–5 步：校验与生成

```bash
python3 "$RUN" check model.spec.json
python3 "$RUN" gen model.spec.json -o model.tbl
python3 "$RUN" check model.tbl
```

模型放进项目后，对整个项目再跑一次 `python3 "$RUN" check <项目目录>`。在项目里（上层有 `data/` 目录）还会检查：

- 引用的模型（`moduleTablePath`，按平台规则解析 `$DATA:`、`$APP:`、`$TAPP:`、相对和绝对路径）和 Script 节点的脚本（`data/script/<名>.action(.ts)`）确实存在；
- 每个加工有 id 为 `default` 的输出节点；
- `.action.ts` 的脚本规则：不读 `context.params`，常量不写死日期，加工脚本（定义了 `onProcessData` 的数据流脚本节点）有预览减量，批处理循环检查 `isCanceled()`，`BELONGTOID` 归并链有口径联动注释。`onQueryData`/`onUpdateData` 这类模型拓展点脚本不按加工脚本要求。
- 不归你维护的代码（例如平台自带脚本）用 `--exclude <路径>` 跳过，可重复给出；路径不存在会报用法错误。

任一步退出码不为 0 就停下来修复。修复时按每条诊断的 `path`（JSON Pointer，如 `/nodes/0/inputs/0`）和 `rule`（如 `graph.danglingInput`）回到规格里改。读报告时看 `scope`：`spec`、`dataflow`、`script` 表示确实校验过；`skipped` 表示文件不是 DataFlow 模型（如 App 录入表），**没有校验**，不能算通过。

## 修改或读懂存量模型

```bash
python3 "$RUN" decompile existing.tbl -o existing.spec.json
```

反解结果的顶层键说明了形态：

- `nodes`：可编辑的生成形态，但其中仍可能有 raw 节点。
- `archive`：DataFlow 保真快照，改字段后**不会**重新推导下游。
- `rawTable`：非 DataFlow 文件的原样快照。

不加修改直接 `gen` 反解结果，得到的 `.tbl` 与原文件内容一致。在反解结果上做小改动后 `gen` 到新路径，其余内容也会保留。不要手写一份新规格去替换存量模型：手写规格里没写的内容（如顶层 `version`、`rollbackEnabled` 等模型属性、节点的 `lastModifyTime`）都会丢失。

如果改动需要重新推导字段，就按来源重建 typed 节点图，不要把 raw 往返当作语义验证。`archive` 形态改动后不会重新推导，加节点、改字段这类改动更适合在设计器里完成。只有 Script 节点的模型，加工逻辑在它引用的脚本里，改脚本，不要重新生成 `.tbl`。

## 交付

只写入用户指定的位置。汇报内容：

- 规格和 `.tbl` 的路径
- 最后一次 `check` 的实际结果（scope、warnings）
- 哪些字段和口径已经由用户确认、哪些是推断
- 还需要用户在平台上核对的外部依赖：引用的模型或数据源路径、原生 SQL、设计器里保存和预览

## 启动器问题排查

- 首次运行会从 CNB Release 下载当前平台的二进制并校验 SHA-256，之后使用缓存。支持 macOS-arm64、Linux-amd64、Windows-amd64。
- 访问不了 cnb.cool 时，二选一（哈希仍按 `release.json` 校验，不一致就拒绝执行）：
  - 有镜像：设置 `TBLGEN_MIRROR=<基础 URL>`，启动器改从 `<基础 URL>/<版本>/<产物名>` 下载。
  - 完全离线：设置 `TBLGEN_BIN=<本地二进制>`，必须是同版本的发布产物。
- `TBLGEN_CACHE_DIR` 可以改缓存目录。HTTP(S) 代理按标准的 `HTTPS_PROXY` 环境变量生效。
- `tblgen launcher: ...` 开头、退出码为 2 的错误来自启动器（网络、哈希不符、缺少 `release.json`），不是规格问题，不要去改规格。
