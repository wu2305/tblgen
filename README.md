# tblgen

离线生成、校验和反解 SuccBI DataFlow `.tbl` 模型、并按平台规则检查整个项目的命令行工具。`examples/` 中的物业预算模型使用虚拟测试数据，不连接真实工作簿或服务。

## 使用

从对应版本的 CNB Release 下载适合当前平台的二进制（Windows 为 `tblgen.exe`），不需要安装 Rust。Release 同时提供 `SHA256SUMS` 校验文件与 `tblgen-skill-<tag>.zip` 技能包，先核对摘要再使用：

```sh
shasum -a 256 -c SHA256SUMS   # Linux/macOS；Windows 用 certutil -hashfile <文件> SHA256
```

把二进制放入 PATH 后，以下命令在仓库根目录运行；`-o` 的目标必须尚不存在。

```sh
tblgen schema
tblgen check examples/budget.spec.json
tblgen gen examples/budget.spec.json -o my-budget.tbl
tblgen check my-budget.tbl
tblgen decompile my-budget.tbl -o editable.json
tblgen new script --table ods_demo --schema ods --extract true \
  --fields "k:C:20,amount:N:18.2:m" -o projects/P/data/tables/ods/ods_demo.tbl
tblgen check projects/P
```

`schema` 返回完整规格和节点、步骤示例。不应把 `archive` 或 `rawTable` 归档的保真往返当作字段推导。`new` 生成脚本型或声明式（ModelTable →（Join）→ Select → Output）模型，`--extract` 决定提取（ETL，写入 `--table`）还是查询逻辑。`check` 接受目录：在项目目录（含 `data/`）内会按平台规则解析模型引用和脚本路径，并检查 `.action.ts` 脚本规则。它只做离线校验，不验证数据库或 SQL 的实际执行。

### Agent 技能包

`tblgen-skill-<tag>.zip` 内含 `tblgen/SKILL.md`、`tblgen/run.py`、`tblgen/release.json` 和 `tblgen/examples/` 下的业务示例。解压后按 `SKILL.md` 的说明调用 `run.py`（macOS/Linux 用 `python3`，Windows 用 `py -3`，用 SKILL.md 所在目录的绝对路径，不依赖当前目录）。`run.py` 按包内 `release.json` 清单从 Release 下载对应平台二进制并校验 SHA-256 后执行；已缓存或设置 `TBLGEN_BIN`/`TBLGEN_CACHE_DIR` 时可完全离线，设置 `TBLGEN_MIRROR` 可改从镜像下载（仍校验 SHA-256），详见 SKILL.md「启动器问题排查」。源码树中的 `skill/tblgen/` 是模板，`release.json` 由打包流程生成，源码树内不能直接运行。

## 构建

| 发行平台 | Rust target | 产物名 |
| --- | --- | --- |
| macOS-arm64 | `aarch64-apple-darwin` | `tblgen-v0.1.1-macos-arm64` |
| Linux-amd64 | `x86_64-unknown-linux-musl` | `tblgen-v0.1.1-linux-amd64` |
| Windows-amd64 | `x86_64-pc-windows-gnu` | `tblgen-v0.1.1-windows-amd64.exe` |

产物名前缀 `tblgen-` 后接发布标签（`vX.Y.Z`），下表以当前版本 v0.1.1 为例。

维护者可运行 `cargo test --locked` 和 `cargo build --release --locked --target <TARGET>`。交叉编译需要相应目标标准库和链接器；发布前在目标系统运行 `schema` 及 `gen → check` 冒烟测试。

三个平台二进制齐备后，用 `python3 scripts/package_skill.py <tag> <dist目录>` 生成技能包 zip（写入 dist 目录，已存在则拒绝覆盖）；它读取 `skill/tblgen/` 下的模板并按二进制实测 SHA-256 生成 `release.json` 清单。

## 按标签发布

维护者先更新 Cargo.toml/Cargo.lock 的版本并合入 main，再从该提交推送同版本的 vX.Y.Z 标签。CNB 的 tag_push 事件运行测试，交叉构建三个目标，在 Linux 上运行生成与校验冒烟，将三个二进制及 SHA256SUMS 上传到对应 Release。普通分支推送不会发布；不要移动已发布的标签。
