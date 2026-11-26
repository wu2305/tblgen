# C5 · 批处理并行

## 交付字段

| 任务 ID | 基线/输入指纹 | 唯一变量 | 修改范围 | 原始结果位置 | 收益与代价 | 等价证据 | 已知盲区 | 保留/放弃及下一步 |
|---|---|---|---|---|---|---|---|---|
| C5 | tblgen `a9065c72c0f85598eb40285947c625350aee0786`（C1 分支头，含 S1 wire 优化）；语料 `succbi_project_container@6920ac514df2d79a3df73a6447cf851abdfc954b` 分支 `xiaoshouyi-corpus`；输入指纹 SHA-256 `03da5fc310c3eecdfc0e27b0a8efdc8445567b4d3f187b0e3fc384cc771f227f`，969 个可检查文件 / 51,307,655 B（828 `.tbl` + 141 `.action.ts`，与 C1 同一指纹） | 每个对比只改 worker 数：C5 二进制 `--jobs 1/2/4/auto`（auto = `available_parallelism` = 8）对照改动前串行二进制；每组先做一次等价性断言再交错计时 | `src/main.rs`：`cmd_check` 的文件循环改为 `run_checks`（有界 `std::thread::scope` 工作池 + `AtomicUsize` 动态取号 + `mpsc` 按输入序号归并）与 `--jobs N` 解析；`tests/check_jobs.rs` 新增 5 个集成测试；`scripts/run_batch_parallel.py` 与两个 JSON 为测量资产；`src/` 其余、`check_file`、`check.rs`、`project.rs`、`script.rs` 零改动，无新依赖 | `benches/batch-parallel.json`（3 case × 5 配置 × 冷/热样本 + 每 case 5 配置等价性哈希）、`benches/batch-parallel-scaling.json`（5 个批次规模 × 4 配置 × 7 交错样本）；复跑命令见下 | 保留。全量 969 文件热缓存墙钟 378.0 → 106.1 ms（auto，−71.9%），冷缓存 485.9 → 143.8 ms（−70.4%）；jobs=4 热 137.3 ms（−63.7%），jobs=2 热 227.8 ms（−39.7%）。代价：CPU 时间 +30.6%（360→470 ms，工作池/缓存局部性开销），峰值 RSS 11.4 → 43.1 MiB（auto，并发解析的 `Value` 树驻留），线程峰值 10（8 workers+主+`time` 包装），fd 峰值 ≤12.5；50 文件小批与单文件无回归（scaling 扫描 auto 均不劣于串行） | 三层证据：(1) 驱动对 full/batch/single 每个 case 断言 5 种配置（旧串行、jobs=1/2/4/auto）的 stdout SHA-256、stderr SHA-256、exit code 逐字节一致，105 个计时样本逐次复核 stdout 哈希与 exit；(2) `tests/check_jobs.rs` 5 个测试覆盖有效/告警/错误文件、invalid JSON、非 flow JSON、`.action.ts`、不存在文件（exit 2）、目录枚举、`--exclude`、项目引用（ModelTable+Script）、混合相对/绝对/`..` 路径、重复参数、`--jobs` 非法值、不可读文件，全部 `jobs=1` vs `2/4/8/auto` 逐字节一致；(3) `cargo test --locked` 全过（16+5+39+43）。stderr 逐文件消息按输入序号回放，流内顺序与串行一致；stdout 为单个 JSON blob，仅在全部检查结束后写出 | 单机 8 核 Linux；RSS 随 worker 数近似线性增长（每 worker 一份解析树峰值），超大单文件批次内仍是 Amdahl 串行段；`/usr/bin/time` 的 CPU 分辨率约 10 ms，小批 CPU 记 0；线程/fd 为 5 ms 轮询峰值，可能略低估瞬时值；未在 arm64/Windows 验证（实现仅用 `std`，`available_parallelism` 有回退）；并行收益依赖文件确实独立——已核对 `check_file`/`check_references`/`check_script` 全部为只读、无全局状态或缓存 | **保留**。端到端墙钟收益 3.6×（热）/3.4×（冷）远超噪声，且各规模批次均无回归；默认即 auto 并行，`--jobs 1` 可回退串行。下一步：C4 已反证放弃，C5 落地后可复评 C3（报告构造占比在并行下相对上升）与 P1；若未来出现超大单文件主导的负载，再考虑节点级并行（本任务明确不做） |

## 复跑

```bash
cargo build --locked --release
python3 scripts/run_batch_parallel.py \
  --base-bin /path/to/pre-C5-tblgen \
  --new-bin target/release/tblgen \
  --root /path/to/succbi_project_container/projects/xiaoshouyi \
  --repo . --corpus-repo /path/to/succbi_project_container \
  --out benches/batch-parallel.json \
  --batch-size 50 --cold 2 --hot 7 --warmup 2
```

冷样本前执行 `sudo -n sh -c 'sync; echo 3 > /proc/sys/vm/drop_caches'`。基线二进制为 C1 分支头 `a9065c7` 的 `cargo build --locked --release` 产物（本报告实验用 `/home/ubuntu/work/tblgen-base`）。

## 实现

- `parse_jobs`：`--jobs N` / `--jobs=N`，在参数列表任意位置生效；缺值、重复、非数字、`0` 均 exit 2。缺省 `std::thread::available_parallelism()`（失败回退 1），worker 数再按文件数封顶。
- `run_checks`：`workers <= 1` 或单文件时走原串行循环（逐文件处理、即时打印 stderr）；否则 `std::thread::scope` 起有界 worker，`AtomicUsize::fetch_add` 动态取下一个文件序号（自动负载均衡，无 rayon 依赖），结果经 `mpsc` 按序号归并进 `Vec<Option<FileOutcome>>`。
- 输出契约不变：报告仍按 `expand_dirs` 的输入顺序收集；每个文件的 stderr 行（仅 I/O 错误有）在归并后按输入序号回放，流内顺序与串行逐字节一致；`had_io_error`（exit 2）与诊断错误（exit 1）的优先级逻辑不变。
- 线程安全依据：`check_file` → `check`/`validate_spec`/`check_script`/`check_references` 全部只读（`fs::read_to_string`、`is_file`、`is_dir`），无 `static` 可变状态、无缓存、无 `unsafe`；`serde_json::Map` 默认 BTreeMap，遍历序确定。

## 结果

单位毫秒；CPU 为 `/usr/bin/time -v` user+system（分辨率约 10 ms）；RSS 为进程峰值（含全部线程）；线程/fd 为 `/proc` 5 ms 轮询峰值（含 `time` 包装进程）。每组 n=5（热）/n=2（冷），配置间交错执行。原始样本见 JSON。

### 全量 969 文件（目录枚举，exit=1）

| 配置 | cache | wall ms（中位） | min–max | CPU ms | RSS MiB | 线程 | fd |
|---|---|---:|---:|---:|---:|---:|---:|
| 旧串行二进制 | cold | 485.9 | 478.7–493.1 | 385 | 11.4 | 2 | 9 |
| 旧串行二进制 | hot | 378.0 | 371.6–393.3 | 360 | 11.4 | 2 | 9 |
| C5 `--jobs=1` | cold | 483.3 | 452.0–514.6 | 390 | 11.6 | 2 | 9 |
| C5 `--jobs=1` | hot | 378.7 | 376.9–459.2 | 360 | 11.6 | 2 | 9 |
| C5 `--jobs=2` | cold | 293.2 | 287.5–298.9 | 445 | 21.9 | 4 | 9 |
| C5 `--jobs=2` | hot | 227.8 | 225.3–239.7 | 400 | 21.9 | 4 | 9 |
| C5 `--jobs=4` | cold | 180.4 | 178.2–182.6 | 455 | 30.6 | 6 | 11 |
| C5 `--jobs=4` | hot | 137.3 | 135.1–159.2 | 420 | 30.1 | 6 | 9 |
| C5 默认（auto=8） | cold | 143.8 | 143.7–143.9 | 550 | 42.0 | 10 | 12.5 |
| C5 默认（auto=8） | hot | 106.1 | 96.1–113.3 | 470 | 43.1 | 10 | 11 |

`--jobs=1` 与旧二进制在噪声内一致（同一代码路径 + 回放式 stderr），证明差异全部来自并行本身。墙钟加速比：hot auto 3.56×、jobs=4 2.75×、jobs=2 1.66×；cold auto 3.38×。CPU 增量来自重复调度与各 worker 同时持有解析树导致的缓存/内存带宽竞争；auto（8 workers）边际收益已递减（4→8 仅再省 31 ms），但仍最快。

### 小批与单文件

| case | 配置 | wall ms（中位） | 说明 |
|---|---|---:|---|
| batch（报告序前 50 文件，exit=0） | 旧串行 hot | 10.6 | 驱动样本；下方 scaling 复测更干净 |
| batch | C5 auto hot | 12.3 | 同上，差异在噪声内 |
| single（诊断最密文件，exit=1） | 全部配置 hot | 10.4–11.3 | 单文件恒走串行分支，无差异 |

驱动的小批样本受 `/usr/bin/time -v` 包装与相邻配置串扰影响；专门的批次规模扫描（`benches/batch-parallel-scaling.json`，perf_counter 直计、7 交错样本）更能说明问题：

| 批次 | 串行 base | jobs=1 | jobs=4 | auto |
|---|---:|---:|---:|---:|
| 前 50 文件 | 4.5 | 4.4 | 2.6 | 2.7 |
| 前 100 文件 | 7.5 | 7.3 | 4.0 | 3.6 |
| 前 200 文件 | 26.7 | 26.4 | 12.5 | 10.7 |
| 前 400 文件 | 162.1 | 165.0 | 53.9 | 35.2 |
| 诊断最密 50 文件 | 55.3 | 55.7 | 23.1 | 20.3 |

所有规模下 auto 均不劣于串行（最小批次也 −40%），不存在需要文件数阈值的回归区间；`--jobs=1` 与旧二进制逐点一致。

## 等价证据明细

- `benches/batch-parallel.json` 每 case 的 `equivalence` 数组：旧串行、`jobs=1/2/4/auto` 五路的 exit code、stdout SHA-256、stderr SHA-256 完全一致；105 个计时样本逐次校验 stdout 哈希与 exit。full stdout 937,138 B，SHA-256 `958595becb847d646775914214d0c7eb3fc06848c681ad3e976732f45844f7f2`，exit `1`（batch exit `0`，single exit `1`）。stdout 字节数与 C1 报告的 931,324 B 不同是因为本机语料克隆路径前缀长度不同（`file` 字段嵌入调用方给的路径拼写），输入指纹一致。
- `tests/check_jobs.rs`（`cargo test --locked` 通过，含既有 16+39+43）：混合树 9 文件（干净 dataflow、带诊断 dataflow、spec、invalid JSON、非 flow JSON、`.action.ts`、项目引用 ModelTable+Script）在 jobs=1/2/4/8/auto 下 stdout/stderr/exit 逐字节一致；目录枚举 + `--exclude` 一致；不存在文件混合有效文件 exit=2 且 stderr 顺序一致；同一文件重复传参、相对/绝对/`./` 混合拼写一致；`--jobs=0/-1/abc/缺值/重复` 均 exit 2；不可读文件（chmod 000）两配置一致（root 下退化为正常检查，断言 scope=dataflow）。
- stderr 语义：串行原本把每个文件的 I/O 错误即时打到 stderr；现在所有文件检查完成后按输入序号回放。每条流内部顺序不变；stdout 仍是单个 JSON blob。无 stderr 内容时两者都为空（full case stderr_bytes=0）。

## 验证

- `cargo test --locked`：通过（16 lib + 5 check_jobs + 39 dataflow + 43 spec）。
- `rustfmt --edition 2021 --check tests/check_jobs.rs`：通过；`src/main.rs` 仅新增代码段符合 rustfmt（文件整体本就不满足，全仓格式化会引入无关 diff，与 C1 处理一致）。
- `python3 -m py_compile scripts/run_batch_parallel.py`：通过。
