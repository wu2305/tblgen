# 校验器误报复核（66f9d34..e3167ac）

基线：tblgen `e3167ac`，本地 BI `6f9b58c5248`。对 `com.succez.bi/src/test/metadata` 的 2175 份文件（平台保存件）和 `zysf-bi` 运行 `check`，
与 `66f9d34` 对比：错误 64 → 90，警告 149 → 5944。新增诊断逐条对照 Java 源码判定。

[落盘契约审核](dataflow-wire-audit.md)中的反例在 `e3167ac` 均已被拒绝或正确生成，本轮不重复。

## 结论与修复

| 规则 | 语料命中 | 判定依据 | 处理 |
| --- | --- | --- | --- |
| `join.condition.qualifier` | 3 错误 | `[汇总].[年月].[年]` 的第二段被当作表限定符。只有链首是别名；typed `gen` 因生成后的 wire 校验也拒绝同一条件。 | 只取链首（`src/model.rs` `exp_table_qualifiers`） |
| `flow.output.partition` | 19 错误 | `DwTableLoader#loadDataFlow` 对 Output 执行 `node.setFields(dw.getFields())`，节点自带字段不被读取。命中项都在顶层：一处角色相反，另一处按 `dbfield` 命名。 | 角色相反或按顶层 `dbfield` 命名降为警告；顶层完全缺失仍报错（该列会丢失，保留 `raw_default_output` 回归） |
| AddField `field.duplicate` | 4 错误 | `DwDataFlowQueryHelper#addField`（:234）遇已有名直接返回，不抛错；隐藏的前驱字段仍在查询中（`setOutput(false)`）。Refactor 改名冲突会在 `QueryImpl#renameField` 抛错，仍为错误。 | wire `check` 降为警告并继续跟踪字段；`gen` 保持拒绝 |
| `warn.field.originalNode` | 5793 警告 | 设计器写入字段诞生的节点别名（`dwbuilder.ts:16569`），可以是新增字段的 Select；`DwTableLoader`（:531-535）按全部节点别名解析。2812 条指向存在的非源节点。 | 只在别名不对应任何节点时警告（剩 2981 条） |
| 空 `fields` 的逐字段步骤 | 0 | `doCompileQuery` 对每个操作字段执行一次步骤，空数组即跳过；此前只有 Filter 被拒。平台语料没有这种步骤。 | `gen` 对 AddField 以外的步骤一律要求非空；`check` 警告 `step.fields.empty` |

修复后同一语料：错误回到 64（均为既有规则），新增警告为 19 条 `warn.flow.output.partition`、4 条 `warn.field.duplicate`。
两个 `examples/*.spec.json` 重新生成后与仓库件逐字节一致。

## 未改动的观察

- `e3167ac` 的基准与脚本不含本机路径，`cargo bench --no-run` 可构建；结论属测量记录，不涉及行为。
- 剩余 2981 条 `originalNode` 警告指向不存在的别名，多为节点改名后的残留；是否可操作见
  [优化交接清单](../benches/optimization-handoff.md) L2。
