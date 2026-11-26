# D1 · 反编译的失败路径

结论：保留实现。PR：<https://cnb.cool/wu2305/tblgen/-/pulls/13>（open，未合并），base `auto/g6-json-ownership-7f3a`，head `auto/d1-decompile-failure-62a1`。

| 任务 ID | 基线/输入指纹 | 唯一变量 | 修改范围 | 原始结果位置 | 收益与代价 | 等价证据 | 已知盲区 | 保留/放弃及下一步 |
|---|---|---|---|---|---|---|---|---|
| D1 · 反编译的失败路径 | 基线 `auto/g6-json-ownership-7f3a` @ `3dba14908ff1875912e7eeddacbe02aee4a5e616`；perf 提交 `b266b2834b2237edf029f879faba314db3021838`，PR head 在其后仅追加测试/证据。输入：xiaoshouyi 语料 828/828 `.tbl`（另 141 `.action.ts` 未计入 decompile），指纹 `fnv1a64:eac885b177de61b2`；fixtures 10 份 `fnv1a64:512dfa706d62fa9a`；synthetic 8 份 `fnv1a64:6c31680f3c31c327` | `rawnode_fallback` 在 body 含顶层 `lastModifyTime` 时返回 `None`，使 `try_generated` 直接失败走既有 archive；unsupported/unknown node 复用同一 fallback | 运行时仅 `src/decompile.rs` +7/-5；回归测试 `tests/spec.rs` +3；证据/报告 `benches/d1-decompile-failure-summary.json`、`benches/d1-decompile-failure-raw-sample.json`、本文 | 本文同目录 summary/raw sample 已提交；完整 raw archive 为 Devin 会话附件 `d1-raw-results.tar.gz`（69 个文件：manifests、synthetic 输入、profile/bench/time JSON），指纹与路径见 summary | 语料全量 5 对 AB/BA 配对中位 wall `688.173699ms → 270.222365ms`，ratio `0.392666`（-60.733%），逐对 `0.382841–0.406476`；324 个 attempted-failure profile wall `664.554ms → 242.230ms`；RSS `37728KB → 37596KB`。fixtures/synthetic 小样本 ratio 约 `1.005/1.014`，噪声级且 typed/raw/malformed 逻辑不变 | profile/bench 输出指纹：corpus 828、fixtures 10、synthetic 8 均 0 mismatch；bench 另比对 kind/bytes/error，corpus 4140 条、fixtures 70 条、synthetic 56 条 0 mismatch；干净 release CLI 抽查 stdout/stderr/exit 逐字节一致；`cargo check --locked --benches` 通过，`cargo test --locked` 103 passed / 0 failed，含 3 个新 decompile `lastModifyTime`/rawNode 回归 | wall 为本 VM release build；未直接测分配字节，RSS 仅代理；语料仍有 4 个 generate_error 与 dimensions 差异 compare_mismatch 未被此证明覆盖；fixtures/synthetic 规模小；未跑 CNB CI | 保留；PR #13 open 未合并。下一步若继续，D2 才处理 normalize/compare/archive 本体成本，或另找可证明的 dimensions mismatch 早退 |

## 计数与分类

真实语料（全量 828 个 `.tbl`，非抽样）基线分类：`raw_table` 504、`compare_mismatch` 278、`generate_error` 46、typed 成功 0。候选后：`raw_table` 504、`try_none` 320、`generate_error` 4。278 个 `compare_mismatch` 的 first diff：274 个为 `node.lastModifyTime: only generated`，4 个为 node `dimensions` 差异。

## 成本证据

语料 A/B 同一 manifest、同一 release 工具链，5 对 AB/BA，`repeats=5 warmup=1`。pair totals 中位：base `688.173699ms`，candidate `270.222365ms`。按基线 outcome 分组的全样本 median 合计：compare_mismatch `621.355544ms → 234.619974ms`（0.377594），generate_error `61.408185ms → 30.986446ms`（0.504598），raw_table `3.452217ms → 3.449856ms`（0.999316）。instrumented profile 显示 attempted failure 324 份：`try_generated 67.139ms → 2.437ms`，`generate 180.690ms → 0.150ms`，`normalize/compare` 约 `64.940ms → 0`，wall `664.554ms → 242.230ms`；剩余成本主要是必须保留的 archive 路径。

## 等价性与回归

- 输出 JSON：profile/bench 两套独立指纹对 corpus/fixtures/synthetic 全部一致；bench 另比对 `output_kind/output_bytes/error`，0 mismatch。
- CLI：干净 release 抽查 22 份（typed generated、archive、rawTable、畸形 DataFlow error）stdout/stderr/exit 逐字节一致。
- 新增回归：typed 节点带 `lastModifyTime` 仍 generated round-trip；rawNode 不带 `lastModifyTime` 仍 generated/rawNode 成功；rawNode 带顶层 `lastModifyTime` 走 archive 且 body 时间戳 verbatim。
- `cargo test --locked`：lib 16、dataflow 41、spec 46，共 103 passed / 0 failed。

## 复跑/证据

```bash
cargo check --locked --benches
cargo test --locked
```

A/B harness 与完整 profile/bench JSON 未提交（体积与临时路径）；提交的是 `d1-decompile-failure-summary.json` 与 `d1-decompile-failure-raw-sample.json`，后者含 synthetic/fixtures 全量 profile 记录与 corpus 11 个代表样本（raw_table、lastModifyTime compare_mismatch、dimensions compare_mismatch、generate_error）的 baseline/candidate 记录。完整原始结果为 Devin 会话附件 `d1-raw-results.tar.gz`。
