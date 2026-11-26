# G1 · Select 重复推演

结论：**保留实现**。validate 侧两个 Select 名称检查合并为单次推演，`simulate_select` 的名称列表跨步骤携带、steps 全程借用，`compute_select_fields` 三条 escape 路径同步借用。wire/诊断输出逐字节不变。

## 交付字段

| 字段 | 内容 |
|---|---|
| 任务 ID | G1 · Select 重复推演（handoff 优先级 1） |
| 基线/输入指纹 | 基线 `8adabd9`（`auto/typed-gen-bench-g0`，仅叠加 bench 工具 commit `2cb1c05`）；候选 `4480d14`（`auto/g1-select-resim`，diff = `66a0add`+`ce7d854`+`4480d14`）。8 案例输入 fnv1a64：small `586f5133ba270301`、wide `ff657ed45ffe7c6f`、deep `d6dc10141409353c`、fan-in `f0ed3ba600a082fb`、fan-out `ba88f4de861622a6`、error-dense `e58bd2ecac9c2db5`、experiment `a0a783f07bf2f58a`、select-heavy `5584f0e167418c9b` |
| 唯一变量 | `src/gen.rs` 的 Select 推演实现；bench 工具（新案例 select-heavy、typed_gen.rs 注册一项）两侧一致，不计入变量 |
| 修改范围 | `check_select_step_fields`+`check_select_explicit_vs_derived` → `check_select_names`（单次名称推演，诊断顺序与全部提前返回条件不变）；`simulate_select` 携带 names、steps 借用切片、空 steps 跳过名称镜像；`compute_select_fields` 三处 steps `.cloned()` → 借用。未动 `field_eq`（G2）、`clone_passthrough`（G6）、`check.rs`（G7 独立防线） |
| 原始结果位置 | 24 对 AB/BA bench JSON：`/tmp/g1-ab2..4/`（48 个 `p{1..8}-{base,cand}.json`，打包 `/tmp/g1-ab-raw.tar.gz`）；汇总 `/tmp/g1-ab-pairs.json`（逐对逐 case 中位数）、`/tmp/g1-ab-summary.json`；分配计数 `/tmp/g1-alloc.txt` |
| 收益与代价 | select-heavy（合成放大）：wall −13.6%、graph_semantics −17.4%、field_compute −23.4%，24/24 对阶段为负；experiment（真实 spec）：field_compute −3.8%、graph_semantics −2.6%、wall −0.5%（噪声内）；wide：graph_semantics −97.8%（无步骤 Select 不再白解 1024 名）；分配次数 select-heavy −8.6%/字节 −11.5%，全部案例下降。代价：validate 侧合并函数更长；无已知运行时代价 |
| 等价证据 | 24 对 × 8 案例 wire/diag/input 指纹两侧逐一相同（含 diag_count：experiment 10、error-dense 192、其余 0）；bench 每 run 断言 profile==normal；`cargo test --locked` 98 全过；`cargo check --locked --benches` 过；CLI `check(gen())` 对 small/wide/deep/fan-in/fan-out/select-heavy/experiment 全部 errors=[]（experiment 仅 10 条既有 `warn.extract.dbfield`） |
| 已知盲区 | 单机共享 VM，wall 波动大（个别对 ±30–100%）；select-heavy 为合成放大，真实收益以 experiment 为准且 wall 级收益≈噪声；不覆盖 rawNode/脚本路径（未改动）；`field_eq`/`clone_passthrough` 仍是主要成本（G2/G6 范围） |
| 保留/放弃及下一步 | **保留**。下一步：G3（Join/Union/Distinct/Output 投影）或 G2（`field_eq` 大写分配），均需各自独立 A/B |

## 改动内容

1. **`check_select_names`（validate，`graph_semantics` 阶段）**：原 `check_select_step_fields` 与 `check_select_explicit_vs_derived` 各自调用 `resolve_field_names` 并把同一前驱名列表从头演化到尾两遍；现合并为一次推演：步骤存在性/表达式引用检查与显式 `fields` 多重集比较共享同一 `current` 名列表。提前返回矩阵逐支核对：无 `steps`/空 `steps`/首步 `rawStep` 且无显式 fields 时不再做纯 memo 的名称解析（无诊断效果）；`rawStep`/`Ok(false)`/`Err` 在任一语义下都终止推演且不产生 `field.mismatch`；诊断顺序 = 步骤诊断按步序 + 至多一个 `field.mismatch`，路径/规则/消息逐字未改。
2. **`simulate_select`（`field_compute` 阶段）**：原每步从 `work` 重建 `names`（`field_name` 逐字段 + Vec 分配）仅用于当步冲突检测；因 `evolve_names` 与 `apply_step_mutation` 对 RemoveField/Refactor/AddField 的名称效果逐一等价（work 字段经 `clone_passthrough`/AddField 必有字符串 name，`field{i}` 兜底不可达），改为跨步骤携带同一列表；steps 从 `.cloned()` 改为借用 `&[Value]`；steps 为空时提前返回、不构建 names（避免无步骤 passthrough Select 的新增分配，wide 的 field_compute 回退修复）。
3. **`compute_select_fields`**：三条 escape 路径（unknown-pred、declared+opaque、no-decl+opaque 的 steps 下标）同步借用切片。`apply_escape_steps` 签名本为 `&[steps]`，行为不变。

未采用「validate→build 共享推演结果」：跨阶段共享会破坏 G7 要求的独立防线语义且诊断顺序风险高；本次只做阶段内去重。

## A/B 方法

两棵独立源码树：基线 `/tmp/tblgen-g1-base` = `8adabd9` + cherry-pick bench 工具 commit（与候选树的 `benches/`、fixtures、`Cargo.lock` 逐字节一致）；候选 `~/repos/tblgen` = `auto/g1-select-resim`。两侧各 `cargo bench --locked --bench typed_gen --no-run` 构建后直接运行 bench 可执行文件。24 对 AB/BA（3 轮 × 8 对，奇数对 base→cand、偶数对 cand→base），每 run 全量 8 案例、case 默认 repeats/warmup；表中值为每对 (cand/base−1) 的中位数，括号为 24 对最小/最大。分配用计数全局分配器（临时 test，未提交），每案例 3 次预热 + 20 次 `generate_with_diagnostics`。

## 结果（配对中位数，us）

| case | wall 基线→候选 | Δ | graph_semantics | Δ | field_compute | Δ |
|---|---:|---:|---:|---:|---:|---:|
| select-heavy（合成，18 节点/160 步骤/5 显式） | 6404→5749 | **−13.6%** | 984→828 | **−17.4%** | 2748→2155 | **−23.4%** |
| experiment（真实，38 节点/15 Select/1 显式） | 3856→3779 | −0.5% | 741→720 | −2.6% | 1385→1320 | **−3.8%** |
| wide（1024 字段无步骤 Select） | 5641→5401 | −2.9% | 86→1.7 | **−97.8%** | 1450→1534 | +1.5%（噪声） |
| deep（64 Select×1 步骤） | 1679→1625 | −2.4% | 120→111 | −7.0% | 418→400 | −3.8% |
| fan-out（8 Select×1 步骤+Union） | 893→866 | −2.1% | 18→16 | −6.9% | 284→270 | −2.9% |
| fan-in（无 Select，对照） | 531→522 | −1.8% | — | — | 223→219 | −2.3%（噪声底） |
| error-dense（rawNode 错误，未改路径） | 557→546 | −1.1% | — | — | 36→36 | −1.6%（噪声） |
| small | 38.5→36.3 | −4.1% | 2.7→2.4 | −11.4% | 7.9→6.9 | −11.4% |

逐对范围：select-heavy 的 graph_semantics/field_compute 24/24 对全负（[−23.9,−6.5] / [−34.0,−5.4]）；wide graph_semantics [−99.9,−96.6]；experiment 各阶段中位数为负但 wall 级在噪声内；fan-in（无 Select）wall −1.8% 给出机器漂移底。

## 分配（计数分配器，每次 generate）

| case | 分配次数 基线→候选 | Δ | 分配字节 基线→候选 | Δ |
|---|---:|---:|---:|---:|
| select-heavy | 203672→186196 | −8.6% | 5522538→4886228 | −11.5% |
| experiment | 46877→45597 | −2.7% | 1756778→1676889 | −4.5% |
| deep | 38917→38021 | −2.3% | 1925132→1871884 | −2.8% |
| wide | 145825→143763 | −1.4% | 8807912→8712106 | −1.1% |
| small | 1016→983 | −3.2% | 48783→46390 | −4.9% |

## 等价证据

- 24 对 AB/BA × 8 案例：`wire_fingerprint`/`diag_fingerprint`/`input_fingerprint`/`diag_count` 两侧逐一相同（指纹为 UTF-8 JSON 字节的 fnv1a64；bench 内 profile 路径与 normal 路径先做全等断言）。
- `generate_with_diagnostics` 的 wire JSON 与有序诊断在基线/候选二进制间逐字节一致（由上述指纹相等推出）；`check(gen(accepted_typed_spec))`：7 个有效 fixture 经 CLI `gen`+`check` 全部 `errors: []`，experiment 仅 10 条与 G0 一致的 `warn.extract.dbfield`。
- 语义覆盖：显式投影（select-heavy 5 个显式 Select、experiment 1 个）、连续 AddField/TransformToCalculatedField/Refactor/RemoveField 链、opaque（SplitField 尾步 → declared+opaque escape）、无步骤 passthrough（wide）、无 Select 对照（fan-in）、rawNode 错误路径（error-dense 未触碰）。`exp`、原始血缘（originalNode/originalField/inputNode/inputField）、显式输出顺序（decl 投影路径）、editor hints（Refactor rename 的 step_hints）均由既有测试与指纹一致性覆盖。
- `cargo test --locked`：16+39+43 = 98 全过；`cargo check --locked --benches` 通过。

## 已知盲区

- 单机共享 Linux VM，wall 噪声大：个别配对出现 ±30–100% 离群（experiment wall 最大 +95.7%）；结论依赖 24 对中位数与逐对符号（select-heavy 阶段 24/24 全负），不排除更大样本下 experiment wall 级收益仍不可分辨。
- select-heavy 是按真实步骤形状生成的合成放大案例（非 BI Java/服务端语料）；真实收益应以 experiment 为准：field_compute −3.8%、graph_semantics −2.6%、wall ≈0。报告不把合成收益当作真实收益。
- wire_check/wire_emit 未改（独立 `check(&wire)` 防线保留），其阶段数字仅作噪声参照。
- 未覆盖 `rawNode` Select（error-dense）与多输入 Select 的 validate 合并路径细节；多输入 Select 在代码上仍只取 `inputs[0]`，与原实现一致。

## 复跑

```bash
# 基线树（已验证等价的对照）：git worktree add <dir> 8adabd9 && git -C <dir> cherry-pick 2cb1c05
python3 scripts/build_typed_gen_cases.py benches/fixtures/typed-gen   # 含 select-heavy
cargo bench --locked --bench typed_gen --no-run                        # 两棵树各自构建
# 交替运行 bench 可执行文件，--out 保存 JSON；按 case 取 samples 中位数后做配对
cargo test --locked && cargo check --locked --benches
```
