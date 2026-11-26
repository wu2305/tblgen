# G3 · Join / Union / Distinct / Output 投影 — lookup pre-index A/B

| 字段 | 内容 |
|---|---|
| 任务 ID | G3 · Join / Union / Distinct / Output 投影 |
| 基线/输入指纹 | 基线 `auto/g1-select-resim` @ `b5373c2`；候选 `auto/g3-join-union-lookup-index`。两侧 bench 工具一致（同一 `benches/typed_gen.rs`、`scripts/build_typed_gen_cases.py`、fixtures，含新增 `union-map`）。16 对 AB/BA 的 `wire_fingerprint`/`diag_fingerprint`/`input_fingerprint`/`diag_count` 逐一相同；单 case 指纹见原始 JSON。 |
| 唯一变量 | typed `compute_*_fields` 的查找结构预索引/直接索引：前驱字段名、输入别名、Union 映射行。未改诊断规则/路径/消息/顺序，未改输出 JSON，未动 G2/G4/G5/G6 路径。 |
| 修改范围 | `src/gen.rs`：新增 `ProjectionLookup`（别名表 + 每前驱 `to_uppercase()` 首字段索引）；`resolve_declared_fields`（Output/Distinct/Join/reshaping 共用）改为索引查找；`compute_union_fields` 增加 canonical key/secondary renamed-value 索引与别名缓存；`field_origin` 接收预计算别名。测试：`tests/dataflow.rs` 增 Join 多输入别名/重名/歧义与 Union 别名≠key/首输入空映射/secondary/error path。Bench：`union-map` 合成案例。 |
| 原始结果位置 | `/tmp/g3-ab-raw.tar.gz`（16 对 × 9 case bench JSON）、`/tmp/g3-ab-summary.json`（配对中位数汇总）、`/tmp/g3-alloc-base.txt`、`/tmp/g3-alloc-cand.txt`（计数分配器，3 warmup + 20 次 `generate_with_diagnostics`/case）。 |
| 收益与代价 | 见下表。真实 spec `experiment`：field_compute −4.13%（14/16 对为负），wall −2.05%（12/16）。合成放大：`fan-in` field_compute −45.05%、wall −18.81%（16/16 全负）；`union-map` field_compute −48.96%、wall −24.07%（16/16 全负）。代价：小表/无关路径为噪声；`fan-out` legacy no-map Union field_compute +0.05%（噪声内）；分配字节 fan-out +2.69%、experiment +0.69%（HashMap 开销），分配次数 fan-in −30.14%、union-map −47.16%。 |
| 等价证据 | 16 对 AB/BA × 9 case：wire/diag/input 指纹与 diag_count 两侧逐一相同；bench 每 run 先断言 profile==normal。`cargo test --locked`：100 全过（16 lib + 41 dataflow + 43 spec）。`cargo check --locked --benches` 过。CLI `gen`+`check` 对 small/wide/deep/fan-in/fan-out/union-map/select-heavy/experiment：`check` errors=0；experiment 仅 10 条既有 `warn.extract.dbfield`。新增测试覆盖多输入、别名不同于 key、首输入空映射、重命名 secondary、隐藏行/legacy、同名列、歧义与 error path。 |
| 已知盲区 | 单机共享 VM，wall 噪声大；`union-map` graph_semantics 中位 +14.82%（16/16 为正）但绝对值仅约 +388 ns，相对 field_compute −290 us 可忽略，按噪声/代码布局处理。`fan-out` 无 map 的 legacy Union 收益在噪声内；`small` field_compute 中位 +3.77% 但仅 3/16 对为负，且该路径未命中本次索引。`select-heavy` field_compute −5.80%/15/16 为负，但 Select 链不在 G3 查找路径上，不计入收益。未测 RSS；分配用计数分配器近似。 |
| 保留/放弃及下一步 | **保留**。下一步：可在 G2 做 `field_eq`/`clone_passthrough` 分配，或 G4 topo/graph_semantics；G3 的 Union secondary 索引为惰性构建，若后续出现 secondary-heavy 真实 spec 再复测。 |

## 结果（16 对 AB/BA 配对中位数）

| case | wall Δ（负对/16） | field_compute Δ（负对/16） | 备注 |
|---|---:|---:|---|
| experiment（真实） | **−2.05%**（12） | **−4.13%**（14） | 真实 spec 收益 |
| fan-in（Join 合成） | **−18.81%**（16） | **−45.05%**（16） | Join 前驱字段/别名索引 |
| union-map（Union map 合成） | **−24.07%**（16） | **−48.96%**（16） | Union 映射行索引 |
| fan-out（Union 无 map） | −0.49%（10） | +0.05%（7） | 噪声内 |
| select-heavy | −3.99%（14） | −5.80%（15） | 非 G3 路径，不计入 |
| small | +1.32%（6） | +3.77%（3） | 噪声 |
| wide | +0.54%（7） | −0.24%（8） | 噪声 |
| deep | −0.47%（10） | +1.54%（4） | 噪声 |
| error-dense | −0.33%（9） | −0.93%（12） | 未改路径对照 |

逐对范围（field_compute）：fan-in [−50.24,−39.42]，union-map [−63.58,−47.92]，experiment [−6.84,+12.67]。wall 范围：fan-in [−25.50,−11.60]，union-map [−36.58,−21.97]，experiment [−4.96,+14.79]。

## 分配（计数分配器，每次 generate）

| case | 分配次数 基线→候选 | Δ | 分配字节 基线→候选 | Δ |
|---|---:|---:|---:|---:|
| fan-in | 14565.9→10176.3 | −30.14% | 538431.8→523889.6 | −2.70% |
| union-map | 44787.0→23667.0 | −47.16% | 1246161.0→1206573.0 | −3.18% |
| experiment | 45597.0→44829.0 | −1.68% | 1676889.0→1688376.0 | +0.69% |
| fan-out | 21441.0→20752.0 | −3.21% | 1079754.0→1108774.0 | +2.69% |
| small/wide/deep/select-heavy/error-dense | 不变 | 0.00% | 不变 | 0.00% |

## 复跑

```bash
python3 scripts/build_typed_gen_cases.py benches/fixtures/typed-gen
cargo bench --locked --bench typed_gen --no-run   # 两棵树各自构建
# 交替运行 target/release/deps/typed_gen-* --out p{r}-{base,cand}.json
cargo test --locked && cargo check --locked --benches
```
