# DataFlow 制作端：4.19.0 落盘契约审核

基线：tblgen `66f9d34`，本地 BI `6f9b58c5248`。这是对当前生成路径的审核，不是对两个业务模型结果集等价性的验证。仅运行离线 CLI，未运行 BI 服务、SQL 或提取任务。

## 分析起点与证据优先级

1. **确认生产者。** `src/main.rs:475-486` 中 `new` 先调用 `generate(&spec)` 生成表，`--spec-out` 只是另存同一个 spec。实验目录 `/private/tmp/tblgen-rebuild` 中 `seed.spec.json` 是 5 节点种子，`build_model.py:225-242` 写入最终的 38 节点 `model.spec.json`。不是 `--spec-out` 独自生成了 38 个节点。
2. **确认保存版本。** BI `web/static-file/metadata/metadata.ts:561-562` 的 `tbl.version` 是 `4.19.0`；`web/static-file/dw/dwbuilder.ts:2580-2584,2954` 保存时取此值。产品分支号不是 `.tbl` 版本，本次不以升级器为依据。
3. **分开读、写、执行。** 平台设计器 `DwTableDataFlowNodeBuilder.toJSON` 定义通常落盘的形状；Java `DwTableLoader.loadDataFlow`、`DataFlowNodeImpl.loadNodeInfo` 定义装载；各节点 contributor 定义查询语义。Java 能容忍的形状不一定是设计器会写的形状；反过来，字段清单里有列也不等于查询生成了那一列。
4. **独立对照。** 在 `com.succez.bi/src/test/metadata` 下，仅统计可解析 JSON、`version == "4.19.0"`、`properties.modelDataType == "DataFlow"` 且 `dataFlow.nodes` 为非空对象的 `.tbl`。得到 157 份、800 个节点。另用业务模型 `养修记录.tbl` 对照同类节点，不要求重建件和它节点数相同。仓库自产的 `examples/*.tbl` 和实验目录的 `examples_dump` 不作为平台标准。

## P1：离线校验通过，但后端所需信息缺失或类型错误

| 问题 | 实证及影响 | 修复边界 |
| --- | --- | --- |
| Select / AddField 没写操作字段 | 重建件 8 个 AddField 全无 `steps[i].fields`，涉及七个归类常量和最终 `UUID()`。`src/gen.rs:1433-1451` 注释称会补字段，实际 `3190-3203` 原样复制步骤。Java `DataFlowSelectNodeStepImpl.java:96-109` 对缺项取空列表，`DwDataFlowNodeContributor_select.java:147-164` 在遍历该列表时才添加计算列。按此路径这 8 步不会执行。BI 对照集的 158 个 AddField 都有非空操作字段。 | 对 typed 简写补 `fields: [addField.name]`，对已落盘的缺项报错；不要把已有平台步骤一律改成单元素数组。`tests/spec.rs:535-551` 名称说在检查 AddField，断言读的却是节点级 `fields`，应直接断言步骤和查询的投影。 |
| Union 按限定字段名建键，却按裸字段名查键 | 重建件两路 Union 的 7+3 个普通输出列均无 `inputNode/inputField`。注意装载后的第二步：`DwDataFlowNodeContributorImpl.java:229-258` 克隆节点字段，`DwUtilDataFlow.java:593-603` 对无表达式的普通列补 `inputField=name`，**不补别名**。`DwDataFlowNodeContributor_union.java:64-78` 以映射行首个非空值和对应输入别名建键；`DwTableFieldImpl.java:206-216` 在缺别名时返回裸名。例如 `union_person` 用裸名 `车辆VIN` 找不到 `[branch_direct].[车辆VIN]` 的映射。进入 `sortFields` 的映射分支后，`DwDataFlowNodeContributor_union.java:119-122` 直接对空的查找结果调用 `values.get(queryIndex)`，不是简单跳过 10 列。此为源码路径推断，未实跑 BI。`src/gen.rs:2430-2451` 的行宽校验和 `src/schema.rs:719-725` 的示例都没验证这个键。 | 校验可见非计算列和映射键一一对应；有多路输入时来源别名必须可解析。首个非空值可以来自第二个输入；无映射数组的兼容路径另测。 |
| Agg 节点过滤器的 JSON 类型相反 | 这是额外的最小反例，不是重建件含有 Agg。基于 `node_Agg.json`，`config.filter = "[m01] > 0"` 能生成字符串；改为 `{matchAll, enabled, clauses}` 对象反而得到 `structure.config.type`。`src/model.rs:337-339` 把它注册为标量，`src/gen.rs:634-650` 按标量校验。Java `DataFlowNodeImpl.java:386-391,757-775` 把节点 `filter` 强转为 Map；字符串与装载器不兼容。 | 节点级 `filter` 应建模为条件对象；Select 的 `steps[].filter` 仍是字符串，两者不能用同一规则。覆盖正反两种载荷。 |

## 生成器专项复核：可复现的反例

以下均是离线合成反例，并非声称养修重建件触发了每一项。用当前 `target/debug/tblgen`，将 spec 从 `/dev/stdin` 交给 `gen`，再将其输出交给 `check`。表中 `0/0` 表示两步退出码均为 0 且均无诊断；`0/1` 表示 `gen` 无诊断，但其产物被 `check` 拒绝。Java 一侧只核对装载及查询构建源码。

| 优先级 / 触发条件 | `gen/check` 的实测结果 | 源码及影响 |
| --- | --- | --- |
| P1：Select 删除 `sku`，上游为 `ID, SKU` | `0/0`；Select 和顶层仍列出 `ID, SKU` | 校验不区分大小写 (`src/gen.rs:2045-2047`)，推导却用精确字符串集合 (`2995-3002`)。Java `QueryImpl.java:763-769` 将名字大写后删除；`syncFields` 将没有对应查询列的元数据放回字段清单，并不创建查询列 (`DwDataFlowQueryHelper.java:1672-1682`)。于是声明有 `SKU`，查询却没有。 |
| P1：把 `SKU` 改名为已经存在的 `ID` | `0/0`；顶层生成为 `ID, ID` | `src/gen.rs:3031-3046` 在原位置改名，没检查其他列；Java Select 调 `query.renameField` (`DwDataFlowNodeContributor_select.java:217-225`)，而 `QueryImpl.java:745-755` 遇到重名直接抛错。既要检查步骤执行时的名字，也要保持最终投影唯一。 |
| P1：typed 与 rawNode 的最终 ID 相同 | `0/0`；三个 spec 节点只剩两个 wire 节点 | 例：typed 源 `key:m,id:shared` 声明 `ID, SKU`；随后 `key:shared` 的 rawNode 是 `other.tbl`，只有 `OTHER`；Output 输入 `m`。`src/gen.rs:214-233` 只把 typed ID 放入去重集合，`2742-2751` 后项覆盖前项。产物的 Output 指向 `shared`（已换源），顶层字段却仍是 `ID, SKU`。`DwTableLoader.java:1248-1258` 按最终 map key 装载。 |
| P1：默认出口为 rawNode，或唯一 Output 显式指定非 `default` ID | raw `default` 内有 `ID`：`0/0`，但顶层两分区均为空；typed `id:published`：`0/1`，报 `flow.noDefaultOutput` | `src/gen.rs:2789-2791,3252-3260` 没有为 raw 出口取得字段就返回两个空数组；Java 对 Output 绑定顶层模型字段，而非保留节点自己的那一列 (`DwTableLoader.java:1254-1259`)。非默认 ID 的反例则是另一条路径：未指定运行节点时 Java 查 `default`，查不到抛 `err.dataflow.nooutput` (`DwDataFlowContext.java:172-178`)。生成阶段应区分“已知无列”“无法推导”和“没有默认出口”。 |
| P1：已校验的 `config` 被同节点 `raw` 改写 | `0/0`；`config.moduleTablePath=./m.tbl`、`raw.moduleTablePath=./other.tbl`，最终引用后者 | `src/gen.rs:1923-1933` 只禁固定结构键，`3216-3230` 先合并 config 再合并 raw。后端读最终的 `moduleTablePath` (`DataFlowNodeImpl.java:352-357`)。不应因支持扩展属性就允许两个互相矛盾的数据源声明。 |
| P1：Join 的 spec key 被当作落盘别名 | `0/1`；示例两侧 key 为 `l/r`，真实 alias 为 `left/right`。只改条件的 `leftTable/rightTable` 为 `l/r`，得到两条 `join.condition.alias` | `src/gen.rs:2388-2405` 同时把 key 与 alias 放入合法集合，写出配置时不翻译。Java 直接用 `leftTable/rightTable` 构造关联，执行时按输入别名查关系 (`DataFlowNodeImpl.java:809-815`；`DwDataFlowNodeContributor_join.java:81-90,365-374`)。`inputNodes` 的图标识与条件的表别名不能共用一个校验集合。 |
| P1：DbOutput 增量条件类型写反 | `config.incrementRange` 为字符串时 `0/0` 并写出字符串；改为条件对象数组，`gen` 反而以 `structure.config.type` 拒绝 | `src/model.rs:313-329,409-420` 将此项注册为标量；Java `DataFlowNodeImpl.java:485-493` 将其强转为 List 并逐条加载条件。与 Agg 节点 `filter` 一样，是 typed 生成允许装载器不接受的 JSON 类型，不是缺少可选键。 |
| P2：Agg / Distinct 接受两条数据输入 | 两种节点均 `0/0`，产物保留两个输入 ID | `src/gen.rs:1782-1790` 只要求至少一个输入；Java 对这两种节点用 `getPreNodeExe()` 和 `getPreQuery()`（`DwDataFlowNodeContributorImpl.java:186-192,274-280`；`DwDataFlowNodeContributor_agg.java:30-35,57-60`；`DwDataFlowNodeContributor_distinct.java:46-51,110-118`）。两个正常返回查询的输入不会按两个数据集做聚合/去重；前驱可被编译，不等于其数据参与结果。不要把这个结论推广给本来就支持多输入的 Update、Union 或 Join。 |

优先验收：把 `gen(spec)` 的最终字段集合与 BI 查询列进行对照，而不只与反解出的 JSON 对照。图标识、别名、默认出口、来源配置冲突，在生成阶段明确拒绝或无歧义规范化；`gen` 成功的 typed 产物至少应通过 wire `check`。wire `check` 通过仍非充分条件，上表的 `0/0` 案例必须用独立断言覆盖。

## P2：制作端写出的键与保存格式不一致

- **Join 条件有两套真值。** 重建件 5 个 clause 都同时携带 `exp` 和 `leftExp/operator/rightExp`；业务对照件是 4 个结构化条件 (`clauseType: 0`) 和 1 个表达式条件 (`clauseType: 5`)。`src/schema.rs:696-706` 示范双写，`src/gen.rs:857-878` 又禁止 `clauseType` 并强制左右表达式。实测平台的 `{clauseType:5, exp:...}` 被报 3 处错误。Java `ClauseFilterImpl.java:88-110` 优先 `exp`：本样本的两份条件相符，但只改结构化一侧会留下旧条件。应区分两种表示，拒绝冲突；不要把编辑器的 `clauseType` 说成 Java 执行必需。
- **单输入不应套用多输入的来源规则。** 重建件 Select 的 74 个继承字段和 Output 的 10 个字段都被加了 `inputNode` (`src/gen.rs:2854-2876,3124-3129`)；平台普通前序继承会先删它 (`dwbuilder.ts:20470-20482`)。反过来，不应只因 ModelTable 的普通字段缺 `inputField` 就报错：执行器克隆时先以显示名补齐；`DwDataFlowNodeContributor_dwtable.java:178-209` 先匹配上游字段名，再以 `dbfield` 回查，只有两者均不匹配才移除字段。真实上游物理名与显示名不同，本身不足以推出来源匹配失败。引用文件存在也不证明字段匹配；物理输出列的命名是另一个问题。
- **请求专用字段被持久化。** 重建件 15 个 Select 都有 `stepCount`，平台对照集 178 个 Select 均未写。`dwbuilder.ts:20823-20845` 在分步查询请求里同时发送步骤子集和总数；Java `DataFlowNodeImpl.java:415-418` 读完整步骤时本来就会补足总数。`src/gen.rs:3204-3214` 总是写 `max(显式值, 步骤数)`；若显式值过大，`DwDataFlowNodeContributor_select.java:266-269` 的末步节点过滤条件不会触发。保存和查询请求应分开处理。
- **默认版本缺失，而不是样本版本过低。** 重建 spec 显式写了 `4.19.0`。`src/new.rs:122-127`、`src/gen.rs:2760-2765` 在未指定时完全不输出 `version`；无版本的 Agg 示例实测也没有版本键。给新建/typed 生成设当前保存默认值，显式版本保留；不要给 `archive` / `rawTable` 保真路径静默补版本。

## 不应当作错误的差异

- **三份字段视图不是三个无用副本。** 平台 `dwbuilder.ts:14066-14071,20711-20717` 本身同时写节点 `fields` 和维度/度量视图；对照集 800 个节点全部既有非空 `fields`，也有至少一个非空分区。Java 普通节点从 `fields` 装载 (`DataFlowNodeImpl.java:419-425`)；表级字段从顶层 `dimensions/measures` 装载，Output 再绑定模型字段 (`DwTableLoader.java:1424-1429,1254-1262`)。不可全局删任一侧。可单独治理的是重建件的 15 个空分区和节点 `fields` 中 46 个显式 `isDimension:false`；保存器会清理为假的字段角色 (`dwbuilder.ts:2677-2696`)。不要递归删除所有 false 或所有空数组。
- **可读节点 ID 不等于图损坏。** 平台新建普通节点多用 UUID、首个 Output 用 `default` (`dwbuilder.ts:19233-19245`)。tblgen 默认用 spec key 作普通节点 ID (`src/gen.rs:214-229`)；Java 装载器按 `nodes` 的 map key 建节点。本次要校验 `inputNodes` 指向的 ID、Join/Union 引用的别名与默认出口，不是把所有字符串替换成 UUID。`moduleTablePath` 的拼写也确实由 Java 读取 (`DataFlowNodeImpl.java:352-357`)。

## 回归闸门

1. 以平台保存的 4.19.0 最小 fixture 作对照，不以 tblgen 对自身的 round-trip 作执行证明。优先使用合成或脱敏 fixture，不将业务模型复制进仓库。
2. 分别测试 `gen(spec)` 写出的载荷、`check(wire)` 能否拦截已知反例、BI 构建查询后的投影和谓词。首批覆盖 AddField 常量及 UUID、两路 Union（首路为空及重命名）、节点与步骤两种 filter、两种 Join clause。
3. 再对单输入继承、空分区、默认值和默认版本做保存形状断言。标准模型中的未知字段、层次和历史版本仍须能保真往返；typed 与 archive 两种结果分别统计。

本轮 CLI 证据：当前源码重建的 CLI 对最终 spec 及 `.tbl` 均报 0 错误、0 警告，`gen model.spec.json` 与交付件逐字节一致。重建件反解得到 38 个 typed 节点；业务对照件及 BI 自带 `FACT_USER_ANA_NEW_USER.tbl` 均退回 `archive`。`src/decompile.rs:57-65,325-338,399-418` 以本工具能否精确再生为门槛，缺空分区或省略 false 等标准形状会限制 typed 表示。归档保真是有意设计，不是后端不支持这些表，也不是制作端通过了独立验证。

## `new --spec-out` 复核与修复顺序

对照实验目录的留存文件：`/private/tmp/tblgen-review/so/out2.spec.json` 和 `seed.spec.json` 除 `properties.dbTableName` 外 JSON 相同；种子 5 节点、最终交付件 38 节点。`out1.tbl` 实际是 `RemoveField` 加 `stepCount:1`，未见空维度/度量分区；`out2.tbl` 才同时出现 `steps:[]`、`stepCount:0` 和空 `measures`。`new` 与 `gen` 共享写出逻辑，但不能把制作者后来手写的每个键都归给种子。

1. **修执行语义，不要把显式来源全部删除（P1）。** `src/new.rs:187-193,222-241` 对直接 ModelTable 输入写四个血缘键；实验 `build_model.py:16-20` 把同一模式用于多级 Join。以 `维修工单号` 为例：`flt_main` 已记录原点 `src_main`，`join_labor` 把直接输入和原点都写成了 `flt_main`。应把直接来源与沿继承链传播的原点分开；新计算字段以产生它的节点为边界 (`dwbuilder.ts:16694-16719`)。种子两输入都有 `车辆VIN`，当前 DSL 取第一个匹配项；仅凭列名无法知道业务要左列还是右列。Join 在同名或改名时保留显式来源，Union 按映射行绑定，普通单输入 Distinct 同名字段可继承，不要求统一补 `inputNode`。对不可判定的列给诊断，不臆造原点。与原 P1 中的 AddField、节点 filter 一起以查询语义验收。
2. **明确物理输出列契约（P2）。** DSL 可继承上游 `dbfield`，不能单独覆盖输出物理名。无冒号的 `name=dbfield` 被当作完整上游名字查找 (`src/new.rs:37-42`)，带 `:TYPE` 的显式形式在 `235-240` 被拒；script 的显式字段语法则要求 ASCII 标识符。DataFlow 表级字段没有 `dbfield` 时后端回退到 `name` (`DwTableLoader.java:1884-1893`)，这不是后端禁止中文列名。建议支持只覆盖 Output 的物理名、保持上游类型和匹配键不变；`--extract true` 时对未指定且不符合目标命名约定的有效物理名提示，不盲目拼音化，不普遍拒绝中文。
3. **补 typed 默认版本，但区分检查与实际升级（P2）。** 本地 `/private/tmp/tblgen-review/corpus.json` 中 1,062 个不同路径均可解析且带版本；其中 614 份是 `4.19.0`，该清单并非本文限定的 BI 测试集。默认值仍取当前 `tbl` 文件类型注册值，不取样本最大值或产品号。`DwTableLoader.java:167-186` 受仓库就绪、文件 ID、对象存在及文件类型限制；即便转交 `upgradeFileContent`，没有匹配处理器也原样返回 (`MetaServiceRepositoryImpl.java:406-423`)。不能断言每次加载都会执行格式迁移，也不能承诺补 `4.19.0` 一定绕过分派。
4. **最后做保存形状清理；把字段发现单列。** 对无操作 Select，`src/new.rs:275-276,287-293` 写 `steps:[]`；即使 seed 省略，`src/gen.rs:3190-3214` 仍会写空数组和计数。应修共同的 emitter。平铺表按维度后度量读取 (`src/new.rs:164-178`) 与 Java 的表级字段顺序一致 (`DwTableLoader.java:1458-1484`)；和上游 Output 节点 `fields` 顺序不同不等于丢失标准模型顺序。更实质的独立问题是 `read_input` 只扫一层，遇到字段目录时不收集 `children`，而 Java 会递归 (`DwTableLoader.java:1738-1748`)；回归用例应断言列存在，不只是比较顺序。
