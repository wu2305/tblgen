# 前驱字段索引 spike

**结论：保留按需索引。** 这次只把 wire 血缘检查移到 `check/lineage.rs`，
并让一次 `check` 中同一个前驱的字段集合只建一次。没有改生成器、校验规则、遍历顺序或公开 API。

## 静态证据

旧实现对每个需要验证的 `inputField` 遍历直接前驱的 `fields`、执行 Unicode 大写转换、构建集合，然后丢弃。
按 1,062 个真实文件的实际引用计数，旧路径需要建 **63,848** 次集合，按文件和前驱 ID 去重只需 **3,871** 次；
建集合时访问的字段项从 **3,038,777** 降到 **89,627**。这是根据 wire 输入和分支条件计算的工作量，不是采样器计数。
缓存只在单次检查内有效，键是解析后的 wire ID，不是别名；字段匹配、不可验证来源、已知空集合的行为不变。
代价是多个已使用的前驱集合可能同时驻留，直到当前文件检查结束。

## A/B 结果

同一 macOS arm64 机器、相同 lockfile 和 release 配置；基线是拆分前含上一轮修复的工作区，**不是**裸 `HEAD`。
每个负载先各预热一次，再交替进行 8 对 AB/BA。表中时间为各版本中位数；
变化率是逐对计算 `(after / before - 1)` 的中位数。原始样本、输入指纹和工具链见 `lineage-spike.json`。

| 负载 | 指标 | 基线 | 索引版 | 配对变化 |
|---|---|---:|---:|---:|
| 实验模型，38 节点 | 单次 `check(&Value)` | 0.642 ms | 0.519 ms | −19.6% |
| 真实语料，1,062 个文件 | 全批纯 `check` 之和 | 368.7 ms | 106.4 ms | −71.3% |
| 同一语料，实际 `tblgen check` | 全批墙钟时间 | 660.0 ms | 396.7 ms | −39.9% |
| 1 列 | 单次 `check` | 4.1 µs | 4.2 µs | +3.4% |
| 32 列 | 单次 `check` | 0.151 ms | 0.042 ms | −72.8% |
| 256 列 | 单次 `check` | 8.112 ms | 1.046 ms | −87.1% |
| 1,024 列 | 单次 `check` | 130.19 ms | 14.26 ms | −88.9% |

合成样本固定为 `ModelTable → Select → Output`，只改变字段宽度，计时整个 wire checker，
不把结果归作血缘函数的独占耗时。真实语料的纯检查和实际 CLI 均 8/8 对更快。单列表没有获益。
流式语料驱动的进程峰值 RSS 中位数为 **16.20 → 16.28 MiB**（macOS `/usr/bin/time -l`）；
它不是索引的独占内存，也不是对其他图形状的上界。

对照时先比每个文件的**完整有序诊断**（含路径、规则、消息、证据来源），再计时。
实际 CLI 还包括读盘、JSON 解析、项目引用检查和报告序列化；两版均退出 `1`，
完整标准输出逐字节相同。这里的等价是原有诊断没有变化，不是语料零错误。

## 复跑

1. 准备两个独立源码目录，使用同一 `Cargo.lock` 和 `benches/wire_check.rs`。本轮精确基线的
   `src/check.rs` SHA-256 已记录在样本中。仅复验缓存时，也可用当前代码的副本作对照，
   在副本里删掉 `names_by_pred`，把集合查询恢复为 `let names = names_of(src_node);`。
2. 在两棵树分别执行 `cargo build --locked --release --bench wire_check --bin tblgen`。
   从各自 `target/release/deps/` 选择本次构建的 `wire_check-<hash>` 可执行文件（不是 `.d`）。
3. 用基线 CLI 将 `tests/fixtures/experiment.spec.json` 生成为同一份临时 `.tbl`。语料清单可选，
   格式为本机 `.tbl` 路径的 JSON 数组。运行：

   ```bash
   python3 scripts/compare_wire_check.py "$BEFORE_BENCH" "$AFTER_BENCH" \
     --experiment "$EXPERIMENT_TBL" --corpus "$CORPUS_JSON" > samples.json
   ```

   驱动一次只读一个文件，JSON 解析不在 `elapsed_ns` 内；每个进程的 `wall_ns` 含读盘解析。
   单文件数值须将 `elapsed_ns` 除以 `repeats`。`rss_bytes` 只在 macOS 采集。
   要复验表中**实际 CLI**一行，将同一清单的所有路径作为独立 argv 一次性交给各版
   `tblgen check`，捕获并比较 stdout 和退出码，按相同配对顺序测进程墙钟时间。

后续的[输出分区与诊断路径 spike](partition-spike.md)验证了宽表的下一个热点。
要优化 typed `gen`，需另建以 spec 为输入的基线；以上结果不代表生成速度。

[功能消融实验](check-ablation.md)分别测量关闭八组校验和撤回三项性能优化的影响。
