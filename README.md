# tblgen

离线生成、校验和反解 BI DataFlow `.tbl` 模型的命令行工具。`examples/` 中的物业预算模型使用虚拟测试数据，不连接真实工作簿或服务。

## 使用

把适合当前平台的二进制放入 PATH（Windows 为 `tblgen.exe`），不需要安装 Rust。以下命令在仓库根目录运行；`-o` 的目标必须尚不存在。

```sh
tblgen schema
tblgen check examples/budget.spec.json
tblgen gen examples/budget.spec.json -o my-budget.tbl
tblgen check my-budget.tbl
tblgen decompile my-budget.tbl -o editable.json
```

`schema` 返回完整规格和节点、步骤示例。不应把 `archive` 或 `rawTable` 归档的保真往返当作字段推导。`check` 只校验离线结构，不验证外部资源或 SQL 实际执行。

## 构建

| 发行平台 | Rust target | 产物名 |
| --- | --- | --- |
| macOS-arm64 | `aarch64-apple-darwin` | `tblgen-v0.1.0-macos-arm64` |
| Linux-amd64 | `x86_64-unknown-linux-musl` | `tblgen-v0.1.0-linux-amd64` |
| Windows-amd64 | `x86_64-pc-windows-gnu` | `tblgen-v0.1.0-windows-amd64.exe` |

维护者可运行 `cargo test --locked` 和 `cargo build --release --locked --target <TARGET>`。交叉编译需要相应目标标准库和链接器；发布前在目标系统运行 `schema` 及 `gen → check` 冒烟测试。

## 按标签发布

维护者先更新 Cargo.toml/Cargo.lock 的版本并合入 main，再从该提交推送同版本的 vX.Y.Z 标签。CNB 的 tag_push 事件运行测试，交叉构建三个目标，在 Linux 上运行生成与校验冒烟，将三个二进制及 SHA256SUMS 上传到对应 Release。普通分支推送不会发布；不要移动已发布的标签。
