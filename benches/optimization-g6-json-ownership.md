# G6 · JSON 所有权/内部表示 — emit 消费 computed fields A/B

| 字段 | 内容 |
|---|---|
| 任务 ID | G6 · JSON 所有权/内部表示 |
| 基线/输入指纹 | 基线 `auto/g3-join-union-lookup-index` @ `677a2b46257021038ac46c47302b8e1dddf71523`；候选 `auto/g6-json-ownership-7f3a`。16 对 typed_gen + 8 对 alloc_gen 的 `input_fingerprint`/`wire_fingerprint`/`diag_fingerprint`/`diag_count` 逐一相同；单 case 指纹见原始 JSON。 |
| 唯一变量 | typed wire 发射消费 `NodeDesc.out_fields`：`emit_typed_node` 把已算 `fields` move 进节点 body，只为仍需的 partition 与 FileInput `originalFields` 保留有界 clone；默认 Output 顶层 partition 与 extract `dbfield` 警告在消费前一次性读出。未改字段计算、诊断规则/路径/顺序、raw 逃生通道，也未引入 typed IR/整数 ID/字符串驻留。 |
| 修改范围 | `src/gen.rs` only：`emit_generated_wire` 改为 `&mut descs`，发射前取 `default_output_partition`/`extract_dbfield_diags`；`emit_typed_node` 改 `&mut NodeDesc`，用 `out_fields.take()` 后 `partition_fields(&fields)` 并 move `fields` 到 body；FileInput `originalFields` 仍显式 clone；新增 `extract_dbfield_diags`。未新增/修改测试；等价由 A/B 指纹、bench profile==normal、verify `check` 覆盖。 |
| 原始结果位置 | 本会话附件：`g6-ab-raw.tar.gz`（16 对 typed_gen bench JSON + 8 对 alloc_gen JSON + verify base/cand JSON）、`g6-ab-summary.json`、`g6-rss-ab.json`、`g6-verify-base.json`、`g6-verify-cand.json`、`g6-harness.tar.gz`（临时 harness 副本，未提交）。 |
| 收益与代价 | 见下表。主指标 wire_emit 配对中位：wide −42.05%、fan-out −40.79%、select-heavy −39.42%、deep −38.09%、union-map −33.68%、fan-in −32.49%、experiment −30.67%、small −17.58%、select-rawstep −14.92%、error-dense −5.69%。分配字节/次数每次 `generate_with_diagnostics`：wide −26.66%/−24.22%、fan-out −23.94%/−20.72%、deep −20.65%/−16.84%；峰值 RSS 中位 15104→12236 KB（−18.99%，5 对）。代价：error-dense 负例仅 −1.33% 字节；wall 中位数普遍下降但逐对范围含正离群，field_compute/validate/wire_check 多为噪声或随 wall/emit 波动，不作独立收益。 |
| 等价证据 | typed_gen 16 对 AB/BA：每 run 先断言 profile==normal；wire/diag/input 指纹与 diag_count 两侧逐一相同。alloc_gen 8 对：wire/diag 指纹与 diag_count 相同。verify base/cand JSON 完全相同；wire-present accepted specs 的 `check(gen(...))` hard=0（experiment 仅 10 条既有 `warn.extract.dbfield`）；error-dense 为负例，wire=None、gen_hard=192。`cargo test --locked` 100 全过；`cargo check --locked --benches` 过。 |
| 已知盲区 | 单机共享 VM，wall 噪声大；分配字节按 `Layout::size()` 求和，非峰值 RSS（RSS 另用 `/usr/bin/time -v` 5 对，进程峰值含解析与 allocator 保留）；临时 harness 未提交；`select-rawstep` 很小，阶段中位数噪声相对大；未测 CLI/服务端执行；未做 typed IR/整数 ID/字符串驻留。 |
| 保留/放弃及下一步 | **保留**。下一步：G6 剩余候选（`prepare_nodes`/`partition_fields`/`clone_passthrough` 的只读借用或一次性 move）需重新抓分配剖面后单变量评估；轻量 typed IR/整数 ID/字符串驻留仍按 handoff 最后评估。 |

## 结果（16 对 typed_gen AB/BA 配对中位数）

| case | wall Δ（负对/16） | wire_emit Δ（负对/16） | field_compute Δ（负对/16） | validate Δ（负对/16） | wire_check Δ（负对/16） |
|---|---:|---:|---:|---:|---:|
| wide | **−22.62%**（15） | **−42.05%**（16） | −1.33%（9） | −18.49%（14） | −0.82%（10） |
| fan-out | **−16.50%**（15） | **−40.79%**（16） | −1.04%（10） | −13.88%（16） | +0.46%（7） |
| select-heavy | **−10.00%**（12） | **−39.42%**（16） | −3.87%（13） | −9.06%（15） | −1.83%（11） |
| deep | **−15.52%**（15） | **−38.09%**（16） | −2.51%（15） | −13.26%（16） | −1.17%（12） |
| union-map | **−16.09%**（16） | **−33.68%**（16） | −2.81%（14） | −17.23%（16） | −2.74%（12） |
| fan-in | **−14.67%**（16） | **−32.49%**（16） | −0.48%（9） | −9.61%（16） | −0.15%（8） |
| experiment | **−4.18%**（15） | **−30.67%**（16） | −1.60%（13） | −2.37%（14） | −0.53%（10） |
| small | −5.49%（15） | −17.58%（16） | −1.38%（9） | +2.09%（5） | +1.67%（7） |
| select-rawstep | −2.53%（11） | −14.92%（15） | +1.77%（4） | +5.06%（3） | +3.86%（4） |
| error-dense | −8.22%（15） | −5.69%（15） | −1.50%（12） | −37.42%（16） | −0.91%（12） |

逐对范围（wire_emit）：wide [−60.96,−39.44]，fan-out [−58.49,−33.85]，select-heavy [−50.86,−35.17]，deep [−51.08,−33.65]，union-map [−44.59,−25.92]，fan-in [−43.57,−24.58]，experiment [−42.59,−26.10]，small [−31.58,−1.48]，select-rawstep [−24.99,+10.70]，error-dense [−19.14,+14.92]。wall 逐对范围含正离群（experiment [−31.11,+107.32]、select-heavy [−58.95,+110.14]、wide [−69.51,+66.08]、fan-out [−40.96,+38.38]），按噪声处理；完整 min/max 见 `g6-ab-summary.json`。

## 分配（计数分配器，每次 generate；8 对中位数）

| case | 分配次数 基线→候选 | Δ | 分配字节 基线→候选 | Δ |
|---|---:|---:|---:|---:|
| wide | 143763.0→108944.0 | −24.22% | 8712106→6389354 | −26.66% |
| fan-out | 20752.0→16453.0 | −20.72% | 1108774→843366 | −23.94% |
| deep | 38021.0→31619.0 | −16.84% | 1871884→1485388 | −20.65% |
| union-map | 23667.0→19927.0 | −15.80% | 1206573→987585 | −18.15% |
| fan-in | 10174.9→8258.7 | −18.83% | 523886.9→412365.3 | −21.29% |
| experiment | 44829.0→41344.0 | −7.77% | 1688376→1498806 | −11.23% |
| select-heavy | 186196.0→173744.0 | −6.69% | 4886228→4039936 | −17.32% |
| small | 983.0→872.0 | −11.29% | 46390→39733 | −14.35% |
| select-rawstep | 1561.0→1411.0 | −9.61% | 96616→83807 | −13.26% |
| error-dense | 15498.0→15304.0 | −1.25% | 883553→871841 | −1.33% |

## 峰值 RSS

`/usr/bin/time -v` 包住完整 `examples/alloc_gen` 运行（全部 case、默认 repeats/warmup），5 对交替：基线中位 15104 KB，候选中位 12236 KB，Δ −18.99%。候选单次 RSS 样本 12232–12240 KB，基线稳定 15104 KB。该数字是进程峰值，含 spec 解析与 allocator 保留，不等同于纯 `generate_with_diagnostics` RSS。

## 复跑

```bash
# 基线树 /home/ubuntu/repos/tblgen 的父提交 677a2b4 与候选分支各自构建：
cargo bench --locked --bench typed_gen --no-run
cargo build --locked --release --example alloc_gen   # harness 见 g6-harness.tar.gz
# 交替运行 target/release/deps/typed_gen-* --out p{r}-{base,cand}.json（16 对）
# 交替运行 target/release/examples/alloc_gen --out p{r}-{base,cand}.json（8 对）
cargo test --locked && cargo check --locked --benches
```
