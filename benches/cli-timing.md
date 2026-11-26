# C1 · CLI 分段计时

## 交付字段

| 任务 ID | 基线/输入指纹 | 唯一变量 | 修改范围 | 原始结果位置 | 收益与代价 | 等价证据 | 已知盲区 | 保留/放弃及下一步 |
|---|---|---|---|---|---|---|---|---|
| C1 | tblgen `fd1be96a3145c502052e7625616615ce22aa02ba`（S1 基线）；语料 `succbi_project_container@6920ac514df2d79a3df73a6447cf851abdfc954b` 分支 `xiaoshouyi-corpus`；输入指纹 SHA-256 `03da5fc310c3eecdfc0e27b0a8efdc8445567b4d3f187b0e3fc384cc771f227f`，969 个可检查文件 / 51,307,655 B（828 `.tbl` + 141 `.action.ts`） | 每次对比只改一个因素：冷/热页缓存（`drop_caches` vs 预热后顺序跑），或诊断密度分组（low/high dataflow 各 50 文件，另加单文件 low/high） | 新增 `benches/cli_timing.rs`、`scripts/run_cli_timing.py`、`benches/cli-timing.json`、本报告；`Cargo.toml` 仅增加 `[[bench]] cli_timing`；`src/` 无改动 | `benches/cli-timing.json`：80 个计时样本（每 case 3 cold × 2 binary + 5 hot × 2 binary），另有每 case 等价性比对；复跑命令见下 | 收益：把 C2–C5 的投入顺序变成分段证据；代价：bench 仅作测量代理，全量热缓存墙钟比真实 CLI 高 17.5%（442.132 ms vs 376.342 ms），插桩成本不进入产品路径 | 每个 case 先比真实 `tblgen check` 与 bench `report` 的 stdout/stderr/exit；所有计时样本逐次校验 stdout SHA-256 与 exit code。full stdout 931,324 B，SHA-256 `4946429f62f397e777a535acbf25bc0aea121143a8b7b3ed86321c1c6a56f0ba`，exit `1` | 只覆盖单机、单语料分支；冷缓存是 `drop_caches` 后的页缓存冷，不是断电/网络盘；分配与分段 CPU 来自计数分配器/getrusage，含插桩开销；bench 与真实 CLI 不是同一二进制 | 保留 C1 测量资产；下一步优先 C4（JSON 读取/解析），C2/C3 本轮不优先，C5 待 C4 后复评 |

## 复跑

```bash
cargo build --locked --release --bin tblgen --bench cli_timing
scripts/run_cli_timing.py \
  --root /path/to/succbi_project_container/projects/xiaoshouyi \
  --repo /path/to/tblgen \
  --corpus-repo /path/to/succbi_project_container \
  --out benches/cli-timing.json \
  --batch-size 50 --cold 3 --hot 5 --warmup 1
```

冷样本前执行 `sudo -n sh -c 'sync; echo 3 > /proc/sys/vm/drop_caches'`；需要免密 sudo。驱动会自动从 `target/release/deps/` 选择最新 `cli_timing-*` bench 可执行文件。

## 结果

单位均为毫秒；CPU 为 GNU `/usr/bin/time -v` 的 user+system，分辨率约 10 ms，小样本可能显示 0.0。RSS 为进程峰值。

### 全量 969 文件：真实 CLI vs 计时 harness

| binary | cache | wall ms | CPU ms | max RSS MiB | stdout B | exit |
|---|---:|---:|---:|---:|---:|---:|
| `tblgen check` | cold | 502.497 | 410.0 | 11.38 | 931,324 | 1 |
| `tblgen check` | hot | 376.342 | 370.0 | 11.37 | 931,324 | 1 |
| `cli_timing time` | cold | 546.707 | 450.0 | 11.71 | 931,324 | 1 |
| `cli_timing time` | hot | 442.132 | 430.0 | 11.82 | 931,324 | 1 |

冷→热主要省在读盘路径：bench `read_ns` 141.138 → 44.401，`enumerate_ns` 29.009 → 19.121；`json_parse_ns` 195.789 → 199.844、`pure_check_ns` 80.349 → 83.212 基本不变。真实 CLI 全量冷/热墙钟差约 126 ms，主要是 I/O 页缓存，不是解析或纯 check。

### 全量热缓存分段（bench 中位数）

分段和 370.504 ms；占比按分段和计算。`json_parse_ns` 直接测 `serde_json::from_str::<Value>`，不是用 “CLI 减纯 wire” 反推。

| phase | wall ms | CPU ms | alloc MiB | 占分段和 |
|---|---:|---:|---:|---:|
| `json_parse_ns` | 199.844 | 199.861 | 195.225 | 53.9% |
| `pure_check_ns` | 83.212 | 83.259 | 23.224 | 22.5% |
| `read_ns` | 44.401 | 44.429 | 48.931 | 12.0% |
| `enumerate_ns` | 19.121 | 19.120 | 1.379 | 5.2% |
| `locate_ns` | 9.855 | 9.890 | 0.760 | 2.7% |
| `references_ns` | 3.534 | 3.545 | 0.917 | 1.0% |
| `script_check_ns` | 2.629 | 2.632 | 2.004 | 0.7% |
| `report_build_ns` | 3.615 | 3.616 | 6.829 | 1.0% |
| `report_serialize_ns` | 1.634 | 1.634 | 2.000 | 0.4% |
| `report_write_ns` | 0.621 | 0.308 | 0.001 | 0.2% |
| `route_ns` + `split_ns` | 2.039 | — | — | 0.6% |

报告三段合计 5.870 ms；项目定位+引用合计 13.389 ms。bench 全量计数分配器总量 296,313,082 B，峰值 live 6,889,057 B；其中 JSON 解析分配约 195.2 MiB，是最大分配源。

### 少诊断 vs 多诊断

| case | files | diagnostics | stdout B | exit | bench hot wall ms | bench hot CPU ms | `json_parse_ns` ms | `pure_check_ns` ms | report 三段 ms | `references_ns` ms |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| `low_dataflow` | 50 | 0 | 11,111 | 0 | 43.090 | 30.0 | 20.106 | 7.931 | 0.073 | 0.516 |
| `high_dataflow` | 50 | 1,843（18 err + 1,825 warn） | 720,476 | 1 | 72.679 | 60.0 | 30.784 | 15.012 | 4.896 | 0.468 |
| `single_low_dataflow` | 1 | 0 | 234 | 0 | 2.538 | 0.0 | 0.585 | 0.149 | 0.012 | 0.006 |
| `single_high_dataflow` | 1 | 358（1 err + 357 warn） | 135,124 | 1 | 6.878 | 0.0 | 1.992 | 1.126 | 0.946 | 0.004 |

诊断密度主要抬高 `pure_check_ns`、`report_build_ns` 和 `report_serialize_ns`；`references_ns` 在低/高诊断批次里都保持亚毫秒到 1 ms 级。单文件样本受进程启动影响更大，CPU 字段受 `/usr/bin/time` 分辨率限制显示 0.0。

## 结论

下一阶段最该做 **C4 · JSON 读取与 SIMD/缓冲候选**。证据是：全量热缓存下 `json_parse_ns` 199.844 ms，占 bench 分段和 53.9%，约为真实 CLI 热缓存 CPU（370 ms）的 54%；它冷/热几乎不变，说明不是页缓存 I/O，而是 CPU+分配密集；分配约 195 MiB。C4 的前提“解析占显著比例”已被 C1 满足。

- **C2 项目引用 I/O**：本轮不优先。全量 `locate+references` 13.389 ms，占分段和约 3.6%；高诊断 50 文件批次 `references_ns` 仅 0.468 ms。前次消融还显示整组引用会少报 20 条错误，收益/风险不划算。
- **C3 报告构造与输出**：本轮不优先。全量报告三段 5.870 ms；高诊断批次升到 4.896 ms，但仍小于同批次 `json_parse_ns` 30.784 ms 与 `pure_check_ns` 15.012 ms。它随输出字节数增长，适合在 C4 后作为次级优化复评。
- **C5 批处理并行**：暂不先做。真实 CLI 热缓存 CPU 370 ms ≈ wall 376 ms，说明当前是单线程 CPU-bound，文件级并行理论上有墙钟空间；但 C1 没有测多 worker、峰值 RSS/文件句柄、输出归并和失败语义，不能证明端到端收益。建议 C4 降低单文件 CPU/分配后再用同一驱动复评 C5。

不能证明的部分：`json_parse_ns` 包含 `serde_json::Value` 建树与分配，不等于纯语法扫描；本轮没有对比 SIMD 解析器、字节输入或缓冲复用，因此不能承诺 C4 的具体收益幅度。也不能证明并行后仍保持输出顺序、退出码和跨项目引用语义。

## 验证

- `cargo test --locked`：通过（16 + 39 + 43 个测试）。
- `rustfmt --edition 2021 --check benches/cli_timing.rs`：通过。未做全仓 `cargo fmt`，因为仓库既有文件不满足 rustfmt，全仓格式化会引入无关 diff。
- 等价性：`scripts/run_cli_timing.py` 对 5 个 case 均断言真实 CLI 与 bench `report` 的 stdout/stderr/exit 一致；full stdout SHA-256 与历史 CLI 报告一致（`4946429f...`），exit 均为 `1`（low/single-low 为 `0`）。
