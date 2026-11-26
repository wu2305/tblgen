# G2 · 字段匹配与多重集 — Select 显式 fields 计数/索引 A/B

| 字段 | 内容 |
|---|---|
| 任务 ID | G2 · 字段匹配与多重集 |
| 基线/输入指纹 | 基线 `auto/g6-json-ownership-7f3a` @ `3dba14908ff1875912e7eeddacbe02aee4a5e616`；候选 `auto/g2-field-multiset-e27a`。16 对 typed_gen + 8 对 alloc_gen 的 `input_fingerprint`/`wire_fingerprint`/`diag_fingerprint`/`diag_count`/shape 逐一相同；12 个 case 的 `generate_with_diagnostics` `{wire,diags}` payload 也逐字节相同（`diff -qr` 无差异）。 |
| 唯一变量 | Select 显式 `fields` 的名字匹配：`name_multiset_eq` 与 validate/compute 两处显式对派生结果的匹配，由逐项 find/remove 改为按 `to_uppercase()` 计数；projection 由每次线性 `.find()` 改为首个 folded-name 索引。计数不是集合，重名计数必须相等；索引只查成员，输出顺序仍由原声明 `Vec` 决定。未改诊断路径/规则/消息/严重度/来源/顺序，未改输出 JSON。G3 后 Join/Union/Distinct/Output 已由 `ProjectionLookup` 覆盖，G2 剩余变量集中在 Select 显式 fields。 |
| 修改范围 | `src/gen.rs` only：`check_select_names` 的内联 multiset 改为调用 `name_multiset_eq`；`name_multiset_eq` 改 `HashMap<String, usize>` 计数；`compute_select_fields` 在 multiset 相等后建 `sim_by_name: HashMap<String, usize>`（`or_insert` 保留首个 folded 下标）替代每个声明的线性 `.find()`。新增 typed_gen cases `select-explicit-small`/`select-multiset` 与 fixtures；新增 `tests/dataflow.rs::select_explicit_fields_multiset_semantics` 覆盖同序/逆序/重复计数不等/Unicode 大写碰撞。测量 harness `examples/alloc_gen.rs`/`examples/verify_gen.rs` 未提交，见附件。 |
| 原始结果位置 | 仓库内：`benches/optimization-g2-ab-summary.json`、`benches/optimization-g2-alloc-summary.json`、`benches/optimization-g2-verify.json`。会话附件：`g2-ab-raw.tar.gz`（16 对 typed_gen bench JSON）、`g2-alloc-raw.tar.gz`（8 对 alloc_gen JSON）、`g2-verify-payloads.tar.gz`（base/cand `{wire,diags}` payload）、`g2-harness.tar.gz`（alloc/verify harness 副本）。 |
| 收益与代价 | 见下表。核心：`select-multiset` 256 字段显式 Select wall −71.1%、graph_semantics −83.1%、field_compute −81.2%（16/16 对为负）；`select-heavy` field_compute −21.5%（16/16），wall −10.8%（13/16）；`select-explicit-small` graph_semantics −28.7%（16/16）。分配：select-multiset 次数 −83.15%/字节 −27.28%，select-heavy 次数 −9.58%/字节 −1.85%。代价：小表 `select-explicit-small` 分配字节 +374B（+0.50%，次数 −10.65%）；无显式 Select fields 的对照 case（small/wide/deep/fan-in/fan-out/union-map/error-dense）分配基本不变，wall/阶段差按噪声/布局处理。 |
| 等价证据 | typed_gen 每 run 先断言 profile==normal；16 对 AB/BA 的 wire/diag/input 指纹、diag_count、shape 两侧逐一相同。alloc_gen 8 对 wire/diag 指纹与 diag_count 相同。`verify_gen` 输出 `{wire,diags}` 对 12 个 case base/cand `diff -qr` 逐字节一致；accepted specs（除 error-dense 负例）`tblgen gen` 成功且 `tblgen check` 对生成目录 errors 全空（experiment 仅 10 条既有 `warn.extract.dbfield`）。`cargo test --locked` 16+42+43 全过；`cargo check --locked --benches` 过。 |
| 已知盲区 | 单机共享 VM，wall 噪声大；HashMap 计数/索引在极小表可能有字节级开销（已记录 +0.50%）；`to_uppercase()` 仍逐次分配 key，未做字符串驻留；未测 CLI/服务端执行；无显式 Select fields 的对照 case wall 中位数也有 ±，不作为收益证据；未比较 Rust `HashMap` 内部布局，只保证成员/计数语义与首个下标不变。 |
| 保留/放弃及下一步 | **保留**。下一步：若后续 profile 显示 `to_uppercase()` key 分配仍是热点，可评估按字段数自适应阈值或 folded-key 驻留；但需保持 multiset 计数与 first-match 语义，重新做 A/B 与等价验证。 |

## 结果（16 对 typed_gen AB/BA 配对中位数）

| case | 显式 Select fields | wall Δ | graph_semantics Δ | field_compute Δ |
|---|---:|---:|---:|---:|
| select-multiset | 256 | **−71.1%**（16/16） | **−83.1%**（16/16） | **−81.2%**（16/16） |
| select-heavy | 320 | −10.8%（13/16） | −5.6%（12/16） | **−21.5%**（16/16） |
| select-explicit-small | 8 | −9.2%（13/16） | **−28.7%**（16/16） | −12.9%（13/16） |
| select-rawstep | 9 | −6.9%（13/16） | −10.0%（13/16） | −6.8%（13/16） |
| experiment | 10 | −3.8%（11/16） | −1.4%（10/16） | −2.7%（13/16） |
| small | 0 | −9.1%（14/16） | −9.0%（14/16） | −9.2%（15/16） |
| wide | 0 | −2.1%（12/16） | −10.6%（13/16） | −3.8%（11/16） |
| deep | 0 | −4.6%（12/16） | −2.8%（13/16） | −3.8%（13/16） |
| fan-in | 0 | −3.8%（11/16） | −4.5%（12/16） | −3.4%（12/16） |
| fan-out | 0 | −2.7%（12/16） | −5.2%（12/16） | −2.5%（13/16） |
| union-map | 0 | −2.6%（11/16） | +1.7%（5/16） | −1.9%（11/16） |
| error-dense | 0 | −3.4%（9/16） | −0.1%（8/16） | −3.3%（13/16） |

逐对范围（配对中位数相对变化）：select-multiset wall [−76.9,−53.1]、graph_semantics [−85.5,−76.5]、field_compute [−84.9,−72.8]，全部为负；select-heavy field_compute [−36.2,−1.5] 全部为负，但 wall [−28.6,+24.3]、graph_semantics [−24.0,+17.2] 含正离群；select-explicit-small graph_semantics [−43.9,−10.0] 全部为负，field_compute [−28.1,+9.1]、wall [−26.4,+14.9] 含正离群；experiment 各阶段范围约 [−21.6,+21.4]，按小收益/噪声边界处理。显式 Select fields=0 的对照 case 分配计数/字节基本不变，wall 差异不作为收益证据。完整 min/max 与逐对样本见 `benches/optimization-g2-ab-summary.json` 与 `g2-ab-raw.tar.gz`。

## 分配（计数分配器，每次 generate；8 对中位数）

| case | 分配次数 基线→候选 | Δ | 分配字节 基线→候选 | Δ |
|---|---:|---:|---:|---:|
| select-multiset | 236139.0→39788.0 | **−83.15%** | 2769509→2014112 | **−27.28%** |
| select-heavy | 173744.0→157108.0 | **−9.58%** | 4039936→3965168 | −1.85% |
| select-explicit-small | 1719.0→1536.0 | −10.65% | 74212→74586 | **+0.50%** |
| experiment | 41344.0→41199.0 | −0.35% | 1498806→1498471 | −0.02% |
| fan-in | 8259.0→8259.5 | +0.00% | 412366→412367 | +0.00% |
| small | 872.0→872.0 | +0.00% | 39733→39733 | +0.00% |
| wide | 108944.0→108944.0 | +0.00% | 6389354→6389354 | +0.00% |
| deep | 31619.0→31619.0 | +0.00% | 1485388→1485388 | +0.00% |
| fan-out | 16453.0→16453.0 | +0.00% | 843366→843366 | +0.00% |
| union-map | 19927.0→19927.0 | +0.00% | 987585→987585 | +0.00% |
| error-dense | 15304.0→15304.0 | +0.00% | 871841→871841 | +0.00% |
| select-rawstep | 1411.0→1411.0 | +0.00% | 83807→83807 | +0.00% |

小表反效果记录：`select-explicit-small`（8 个显式 fields）分配次数下降 183（−10.65%），但分配字节 +374B（+0.50%），来自 HashMap/uppercase key 的容量与布局；wall/graph_semantics 仍为正向中位数，因此保留但不声称小表无代价。fan-in 的 ±1 次/±2B 为计数噪声。

## 复跑

```bash
# 候选树构建 typed_gen bench；基线树 /home/ubuntu/repos/tblgen-g2-base @ 3dba149 同样构建
cargo bench --locked --bench typed_gen --no-run
# 交替运行 target/release/deps/typed_gen-* --out p{r}-{base,cand}.json（16 对）
# alloc/verify harness 未提交，解压 g2-harness.tar.gz 到 examples/ 后：
cargo build --locked --release --example alloc_gen
cargo build --locked --release --example verify_gen
# 交替运行 target/release/examples/alloc_gen --out p{r}-{base,cand}.json（8 对）
# target/release/examples/verify_gen --out-dir <dir>，再 diff -qr base cand
cargo test --locked && cargo check --locked --benches
```
