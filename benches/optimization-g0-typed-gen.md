# G0 · typed 阶段基准

只交付测量与插桩，不实现 G1–G7 优化。`compile_generated` 仍是一次准备+构建；本次把 `build_generated_collect` 内部拆成字段计算与 wire 发射两个计时段，并在校验、字段计算、发射、wire `check`、诊断回映五段记录纳秒级累计值。正常生成路径传入 `None`，不收集 profile。

## 结论：G1–G7 下一阶段优先级

1. **G1 · Select 重复推演**：最该先做，但先补“按节点类型/操作”的计数。实验 spec 是最接近真实 typed 链路的输入：`field_compute` 36.1%、`wire_check` 28.1%、`validate` 28.5%；`validate` 里 `graph_semantics` 679.9µs，主要落在 Select 名称/显式投影检查路径。深链 `field_compute`+`wire_check` 也占约 52%。假设是 `check_select_step_fields`、`check_select_explicit_vs_derived`、`simulate_select` 重复推演名字列表/steps。
2. **G3 · Join / Union / Distinct / Output 投影**：fan-in `field_compute` 42.1%、fan-out 31.8%，实验 spec 还有 5 Join / 9 Distinct / 2 Union。应在 G1 同一轮补计数后单独立项，避免把 Select 收益算到投影头上。
3. **G6 · JSON 所有权/内部表示**：wide/deep/fan-out 的 `field_compute`+`wire_emit` 合计约 43–55%，clone/分配嫌疑大；但 handoff 要求先抓分配剖面，不能直接改 clone。
4. **G2 · 字段匹配与多重集**：实验 spec 只有 1 个显式 Select fields（10 列），fan-in Join/fan-out Union 有显式声明；证据不如 G1/G3 集中。建议并入 G1/G3 计数（`find_pred_field`、`name_multiset_eq`、投影 `find` 次数），不单独先做。
5. **G5 · 诊断回映**：只在 error-dense 显著（`diag_remap` 23.8%，192 条映射）；有效 spec 的 wire 诊断多为被跳过的 warning，remap 趋近 0。对 gen 吞吐降优先级，若做 diagnostic-heavy `check` 再看。
6. **G4 · 稳定拓扑与图布局**：`topo_sort_ns` 最大 20.0µs（deep，约 wall 的 1.3%），fan-out 6.0µs；没有“大量同时 ready”的用例，当前证据不支持投入。
7. **G7 · 阶段间共享已验证事实**：不是优化项。必须保留独立 `check(&wire)`；实验 spec 有 10 条 wire 诊断但 `mapped_diagnostics=0`（warning 被跳过），说明 wire check 仍在跑且 remap 成本可忽略。

不能证明：单机 wall-clock 中位数不是 CPU/分配剖面；`field_compute` 尚未按节点类型拆分；wide 是无显式 Select fields 的 passthrough，不覆盖 `name_multiset_eq` 最坏情况；error-dense 的非法 step 在 `rawNode` 里，主要打 wire check/remap，不打 typed 字段计算；没有 BI Java/服务端执行证据。

## 复跑方法

```bash
python3 scripts/build_typed_gen_cases.py benches/fixtures/typed-gen
cargo bench --locked --bench typed_gen -- --out /tmp/typed-gen-g0.json
python3 scripts/finalize_typed_gen_g0.py /tmp/typed-gen-g0.json benches/typed-gen-g0.json
cargo test --locked
cargo check --locked --benches
```

计时协议：每个 case 先把 JSON 文本读入并 `serde_json::from_str` 一次，之后所有计时迭代复用同一个 `Value`；先 warmup，再按 case 重复数采样。每个 case 在计时前断言 `generate_with_diagnostics_profile` 与 `generate_with_diagnostics` 的 wire JSON、诊断 JSON 完全一致；不一致则 bench 直接失败不写结果。

阶段口径：`validate_ns` 包含 `spec_validation_ns`、`node_entry_validation_ns`、`prepare_ns`（含 `topo_sort_ns`）、`graph_semantics_ns`；`prepare_ns` 里 `topo_sort_ns` 是子集。`field_compute_ns` 与 `wire_emit_ns` 是 `build_generated_collect` 拆出的两段。`steps` 只统计 typed `nodes[*]` 的 `steps`，不统计 `rawNode` body。

## 输入与指纹

完整 SHA-256 与 FNV-1a 指纹在 `benches/typed-gen-g0.json`；下表为前 12 位。

| case | 输入 | 形状 nodes/fields/steps/edges | wire/mapped 诊断 | sha256 前缀 |
|---|---|---:|---:|---|
| small | `benches/fixtures/typed-gen/small.spec.json` | 3/9/2/2 | 0/0 | `e1c308b9d997` |
| experiment | `tests/fixtures/experiment.spec.json` | 38/246/47/44 | 10/0 | `c80c6ab8e7e1` |
| wide | `benches/fixtures/typed-gen/wide.spec.json` | 3/3072/0/2 | 0/0 | `902c87450ee9` |
| deep | `benches/fixtures/typed-gen/deep.spec.json` | 66/528/64/65 | 0/0 | `1d7fae9ae588` |
| fan-in | `benches/fixtures/typed-gen/fan-in.spec.json` | 6/150/0/5 | 0/0 | `2b0195ef6d65` |
| fan-out | `benches/fixtures/typed-gen/fan-out.spec.json` | 11/352/8/17 | 0/0 | `56e5cf4b722f` |
| error-dense | `benches/fixtures/typed-gen/error-dense.spec.json` | 26/208/0/1 | 192/192 | `8b32273eb8e9` |

## 阶段结果（median）

单位 µs；括号内为占 wall median 的比例。

| case | wall | validate | field_compute | wire_emit | wire_check | diag_remap |
|---|---:|---:|---:|---:|---:|---:|
| small | 34.1 | 8.2 (24.0%) | 7.0 (20.6%) | 6.7 (19.6%) | 10.8 (31.7%) | 0.0 (0.0%) |
| experiment | 3545.1 | 1008.6 (28.5%) | 1280.2 (36.1%) | 195.4 (5.5%) | 997.1 (28.1%) | 0.5 (0.0%) |
| wide | 5438.9 | 1367.0 (25.1%) | 1351.1 (24.8%) | 1387.8 (25.5%) | 1005.1 (18.5%) | 0.0 (0.0%) |
| deep | 1543.6 | 401.1 (26.0%) | 392.7 (25.4%) | 274.4 (17.8%) | 412.0 (26.7%) | 0.0 (0.0%) |
| fan-in | 487.8 | 95.3 (19.5%) | 205.2 (42.1%) | 92.1 (18.9%) | 76.2 (15.6%) | 0.0 (0.0%) |
| fan-out | 826.3 | 162.0 (19.6%) | 263.0 (31.8%) | 174.0 (21.1%) | 189.0 (22.9%) | 0.0 (0.0%) |
| error-dense | 518.0 | 73.0 (14.1%) | 33.4 (6.5%) | 63.6 (12.3%) | 178.1 (34.4%) | 123.4 (23.8%) |

validate 子阶段里最值得注意的两点：experiment `graph_semantics` 679.9µs（validate 的 67.4%），wide `graph_semantics` 1082.8µs（validate 的 79.2%）；deep `prepare_ns` 225.4µs，其中 `topo_sort_ns` 只有 20.0µs，说明深链成本主要不在 G4 点名的有序 Vec 拓扑本身。

## 交付格式

| 任务 ID | 基线/输入指纹 | 唯一变量 | 修改范围 | 原始结果位置 | 收益与代价 | 等价证据 | 已知盲区 | 保留/放弃及下一步 |
|---|---|---|---|---|---|---|---|---|
| G0 | perf baseline `fd1be96a3145c502052e7625616615ce22aa02ba` (`auto/select-step-index-d521`); Cargo.lock sha256 `596ae35bfbd6…`; input sha256 见 `typed-gen-g0.json` | typed gen 阶段插桩与 bench harness；不改变生成语义 | `src/gen.rs` profile hooks；`benches/typed_gen.rs`；`scripts/build_typed_gen_cases.py`；`scripts/finalize_typed_gen_g0.py`；`Cargo.toml` bench target；fixtures 与 `benches/typed-gen-g0.json` | `benches/typed-gen-g0.json` | 收益：分开计 validate/field_compute/wire_emit/wire_check/diag_remap，并覆盖小/实验/宽/深/扇入扇出/错误密集；代价：hidden profile API + benchmark 路径 Instant 计时，正常路径只多几次 `None` 分支 | `cargo test --locked` 98 passed；`cargo check --locked --benches` passed；bench 每 case 断言 profile 输出与 `generate_with_diagnostics` 的 wire+诊断 JSON 一致 | 单机 wall median，非 CPU/alloc；field_compute 未按节点类型拆；无大量同时 ready 的 topo 用例；error-dense 走 rawNode wire 诊断；正常路径 profiler 开销未单独计时 | 保留基准与插桩；下一步先做 G1 Select 重复推演并补 per-op 计数，随后 G3 投影；暂不做 G4/G5 |

## 已知盲区

- 计时是 `Instant` 包住的 wall time，中位数来自单次 release `bench` profile 运行；没有 A/B，因为 G0 不改实现。
- `field_compute_ns` 是聚合段：Select/Join/Union/Distinct/Output/reshaping 都在里面，报告只给方向不给函数级占比。
- wide/deep/fan-out 显示 clone/发射成本高，但未测分配字节或峰值 RSS；G6 必须先补这项。
- error-dense 的 24 个 Select 是 `rawNode` 逃生通道，验证了 wire check/remap 压力，不代表 typed Select 字段计算能承受同等非法 step。
