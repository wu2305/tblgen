# Select 步骤名字索引 spike（S1）

**结论：保留。** 本轮只改 wire checker 的 Select 步骤名字跟踪：有序 `Vec<String>` 仍决定投影顺序和 `evolve_names` 语义，另维护一个 Unicode 大写名字的多重集索引用于“名字当前是否存在”。索引随 RemoveField/Refactor/AddField 的成功变异增量更新；同时去掉每步整份名字列表克隆，Removed 追踪只记录实际被删除的操作数。诊断规则、顺序、表达式扫描和 `evolve_names` 本身不变。

原始配对样本、输入 SHA-256、静态工作量计数、诊断计数和 RSS 样本见 [select-step-spike.json](select-step-spike.json)。合成输入可由 `scripts/build_select_step_cases.py` 重新生成。

## 定位与基线

- 任务 ID：**S1 · Select 步骤**。
- 假设：主要浪费不是表达式词法扫描本身，而是每个引用/操作数在有序名字列表中线性查找，以及每个步骤都克隆整份列表；名称变动是少数事件，存在性索引不必每步重建。
- 反证条件：小输入明显回退、诊断/CLI 输出不能完全一致、或实验模型没有端到端收益。
- 基线源码：`1bc4173bc930d47d94b4eeb825a5547914efd5b2`；`Cargo.lock` SHA-256 `596ae35bfbd60bc991318e54a507ab56ba66f1939ad0f4e89de7f79dc984bfc9`。
- 工具链/平台：Rust 1.99.1 (`58cf8f9c3`)、x86_64-unknown-linux-gnu、Linux 6.8.0-1060-aws；release、locked 构建。
- 输入：9 个确定性合成 Select wire、`tests/fixtures/experiment.spec.json` 生成的 38 节点实验 wire，以及无步骤的 width 对照。所有输入指纹在 JSON 中；本机没有上一轮 1,062 份服务端语料，未验证该语料。

## 工作量计数（每次 `check`）

下列数字按输入静态重放旧分支得到，用于区分工作类型，不是采样器 CPU 计数。“成员扫描”包括操作数/表达式引用在 `cur.iter().any(field_eq)` 中的比较，以及 RemoveField 后为重建 removed 集合执行的二次扫描；“保留的演化比较”是 `evolve_names` 内部的 RemoveField/Refactor/AddField 查找，本轮未改。表达式解析次数和诊断构造次数前后相同。

| 输入 | 成员扫描 | 列表克隆的 String | 保留的演化比较 | 表达式解析 | 诊断 |
|---|---:|---:|---:|---:|---:|
| `filter-1x1.tbl` | 2 | 1 | 0 | 1 | 0 |
| `filter-8x1.tbl` | 6 | 8 | 0 | 1 | 0 |
| `rename-8x4.tbl` | 4 | 32 | 32 | 0 | 0 |
| `filter-256x128.tbl` | 32,896 | 32,768 | 0 | 128 | 0 |
| `filter-1024x64.tbl` | 36,928 | 65,536 | 0 | 64 | 0 |
| `rename-256x128.tbl` | 128 | 32,768 | 32,768 | 0 | 0 |
| `removeadd-256x128.tbl` | 2,105,344 | 32,704 | 32,704 | 64 | 0 |
| `uniqueexpr-256x128.tbl` | 8,256 | 32,768 | 0 | 128 | 0 |
| `missingexpr-256x128.tbl` | 32,768 | 32,768 | 0 | 128 | 128 |
| `experiment.tbl` | 1,842 | 560 | 1,445 | 22 | 10 |

这解释了为什么 Remove/Add 混合链的潜在收益最大：旧实现不仅逐步克隆，还在每个 RemoveField 后对所有旧名字再扫描新列表。表达式本身仍需逐字符解析；S2 可另行评估重复表达式扫描。

## A/B 纯 wire 结果

`elapsed_ns / repeats`，不含 JSON 读盘和解析；每个负载先比较完整有序诊断，再预热，8 对均衡轮换。变化率是逐对 `(after / before - 1)` 的中位数。

| 输入 | 基线 ms | 索引版 ms | 配对变化 | 较快对数 |
|---|---:|---:|---:|---:|
| `width:1`（无步骤对照） | 0.004 | 0.004 | −1.0% | 7/8 |
| `width:32`（无步骤对照） | 0.026 | 0.026 | −0.4% | 5/8 |
| `width:256`（无步骤对照） | 0.193 | 0.190 | −2.1% | 7/8 |
| `width:1024`（无步骤对照） | 0.882 | 0.838 | −4.6% | 6/8 |
| `filter-1x1.tbl` | 0.005 | 0.005 | −1.5% | 7/8 |
| `filter-8x1.tbl` | 0.012 | 0.011 | −1.1% | 7/8 |
| `rename-8x4.tbl` | 0.016 | 0.017 | **+4.7%** | 0/8 |
| `filter-256x128.tbl` | 2.012 | 0.297 | −85.2% | 8/8 |
| `filter-1024x64.tbl` | 3.615 | 1.023 | −71.7% | 8/8 |
| `rename-256x128.tbl` | 2.673 | 1.897 | −28.8% | 8/8 |
| `removeadd-256x128.tbl` | 57.519 | 1.225 | −97.9% | 8/8 |
| `uniqueexpr-256x128.tbl` | 1.362 | 0.293 | −78.6% | 8/8 |
| `missingexpr-256x128.tbl` | 2.248 | 0.357 | −84.1% | 8/8 |
| `experiment.tbl` | 1.329 | 0.994 | −25.5% | 8/8 |

无步骤 width 对照不经过新索引路径，其小幅差异是二进制布局/机器噪声对照，不计入收益。真正的小步骤输入中，1 步 Filter 没有回退；4 步、8 个字段的频繁 rename 回退 4.7%（约 0.8 µs/次），这是维护索引的固定成本。收益侧在实验模型和长链合成输入上都是 8/8 对更快。

## CLI 墙钟

同一文件作为独立 `tblgen check` 进程运行，含启动、读盘、JSON 解析、完整检查和报告输出。8 对 AB/BA，stdout、stderr 与退出码先逐字节/逐项确认一致。

| 输入 | 基线 ms | 索引版 ms | 配对变化 | 较快对数 |
|---|---:|---:|---:|---:|
| `filter-256x128.tbl` | 4.16 | 2.38 | −45.0% | 7/8 |
| `filter-1024x64.tbl` | 7.96 | 5.42 | −31.7% | 8/8 |
| `rename-256x128.tbl` | 4.79 | 4.02 | −15.3% | 8/8 |
| `removeadd-256x128.tbl` | 60.13 | 3.38 | −94.4% | 8/8 |
| `uniqueexpr-256x128.tbl` | 3.29 | 2.33 | −29.4% | 8/8 |
| `missingexpr-256x128.tbl` | 4.54 | 2.51 | −45.5% | 8/8 |
| `experiment.tbl` | 3.47 | 3.10 | −10.0% | 7/8 |

峰值 RSS 用 Linux `/usr/bin/time -v` 各测 5 次。除 `filter-1024x64` 一次中位数差 4 KiB 外，其余中位数相同；该粒度不能证明索引没有额外瞬时分配，只说明未观察到可分辨的进程峰值内存恶化。

## 等价证据

- 所有计时输入先比较完整有序诊断；`compare_wire_check.py` 若 digest 不同会中止。包括 `missingexpr-256x128.tbl` 的 128 条 `warn.exp.ref.notfound` 和实验 wire 的 10 条警告。
- CLI 对 7 个负载逐字节比较 stdout、stderr 和退出码，全部一致。
- `examples/` 与 `tests/fixtures/` 中 10 份既有 `.tbl`（含历史反例、真实 server-saved archive wire）在前后两个 release bench 上的完整诊断输出一致。
- 新增 wire 回归覆盖大小写折叠重复名的多重集语义：Refactor 只改第一个匹配项时剩余重复名仍可被后续步骤引用；RemoveField 仍删除全部大小写折叠匹配项。
- 既有回归覆盖 case-only rename、Unicode `Straße`/`STRASSE`、AddField/Refactor 冲突、rawStep/opaque 停止跟踪、最终字段多重集预算和表达式引用边界。

## xiaoshouyi 真实语料补验（2026-11-26）

语料来源为 `wu2305/succbi_project_container` 的 `xiaoshouyi-corpus` 分支 `projects/xiaoshouyi`，commit `6920ac514df2d79a3df73a6447cf851abdfc954b`；稀疏检出 828 个 `.tbl`。路径 manifest SHA-256 `ccd0e7ccb9a888237eedc2c411dd09a97b98209f4e2fee80a7ae137fc8bce866`。补验仍用同一对 release `wire_check` 二进制：baseline SHA-256 `9d192ea89700b6f49ecfcffc66525b96fed67ea7368c2a6acc180cf57f37c288`，S1 SHA-256 `ae5d09ed10b7a407186a8d5e0ba063c87c1fe11b1bd701ddaa4c7e42627a49e9`；原始配对结果在 [select-step-corpus-ab.json](select-step-corpus-ab.json)（SHA-256 `b62d33de5c62d7c65930aa613aa78b06bb8a6543bd5e04f61abc7985d79b1a36`）。

`taskset -c 0`、12 对 AB/BA、每对 828 个文件各完整纯 `check` 一遍；完整有序诊断 digest 相同。语料配对中位数 **−7.29%**（after/before 0.9271），**10/12** 对更快，配对范围 0.881–1.104。无步骤 width 对照也有约 −1.7% 至 −2.9%，说明仍存在二进制布局/机器漂移；但语料收益幅度更大且方向稳定，补验支持保留 S1。该结果只覆盖这 828 个文件，不外推历史文档提到的 1,062 份，也不推断 typed gen/decompile。

解释上应把对照漂移从名义收益中扣除：本轮更稳妥的净收益估计约为 **4–5%**，而不是完整的 7.29%。12 对样本足以证明方向性收益（10/12 对更快），但每对只完整跑一遍语料，不能作为持续性能回归门槛；它是一次性决策证据，不是可重复执行的 CI bench。

复跑限制：原始 JSON 的 case 键保留了执行机上的绝对路径；语料 manifest 文件本身未入库。若要复验，需要先从上述仓库/commit 稀疏检出 `projects/xiaoshouyi` 下的 828 个 `.tbl`，重建本机路径 JSON 数组，再按 `benches/README.md` 的 corpus 流程执行；路径不同不会改变文件内容 digest，但会使原始 JSON 的 case 键不同。

## 复跑

```bash
mkdir /tmp/select-cases
python3 scripts/build_select_step_cases.py /tmp/select-cases > manifest.json
target/release/tblgen gen tests/fixtures/experiment.spec.json -o /tmp/select-cases/experiment.tbl
python3 scripts/compare_wire_check.py "$BEFORE_BENCH" "$AFTER_BENCH" \
  --experiment /tmp/select-cases/experiment.tbl --pairs 8 \
  --case /tmp/select-cases/filter-1x1.tbl 2000 \
  --case /tmp/select-cases/filter-8x1.tbl 1000 \
  --case /tmp/select-cases/rename-8x4.tbl 1000 \
  --case /tmp/select-cases/filter-256x128.tbl 20 \
  --case /tmp/select-cases/filter-1024x64.tbl 10 \
  --case /tmp/select-cases/rename-256x128.tbl 20 \
  --case /tmp/select-cases/removeadd-256x128.tbl 2 \
  --case /tmp/select-cases/uniqueexpr-256x128.tbl 20 \
  --case /tmp/select-cases/missingexpr-256x128.tbl 20 > samples.json
```

`-o` 目标必须尚不存在。这里的生成命令只用于制备实验 wire；性能数字全部是 wire `check`，不代表 typed gen 或 decompile 加速。

## 交付记录

`S1 | base 1bc4173 + lockfile 596ae35b，合成/实验输入 SHA-256 见 JSON；xiaoshouyi 828 文件 manifest ccd0e7cc | 唯一变量：Select 名字存在性多重集索引并去除逐步整表克隆 | src/check.rs、tests/dataflow.rs、scripts/build_select_step_cases.py、benches/select-step-spike.*、benches/select-step-corpus-ab.json | 原始结果 benches/select-step-spike.json 与 benches/select-step-corpus-ab.json | 实验 wire 纯 check −25.5%、CLI −10.0%；长链合成 −28.8% 至 −97.9%；xiaoshouyi 828 文件纯 check 配对中位数 −7.29%（10/12 对更快）；微小 rename +4.7%（约 0.8 µs），RSS 无可分辨差异 | 有序诊断 digest、CLI stdout/stderr/exit、等价性回归和 cargo test | 真实语料补验只覆盖当前 828 个文件；不推断 typed gen/decompile；表达式重复扫描仍未优化 | 保留；下一步建议 S2 先量化表达式重复扫描，或用 C1 分解真实 CLI 阶段后再排序`
