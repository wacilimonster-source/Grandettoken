# Grandettoken 0.3.16 — 全量测试收尾:Android 用例基线修复与测试工具清理

2026-11-06 的一次全量测试(报告见 `docs/report/test-run-2026-11-06.md`)抓到 1 例失败与 1 处工具腐化,本版收尾。**产品代码零改动**,不含任何行为变化:

- **修复(Android 用例)**:`WidgetFormatterTest` 期望的「月额度(按套餐周期)」括号从全角改回 v0.3.14 定下的句中半角基线 —— 该用例自 v0.3.14 入库起就与实现不一致,从未通过过;当时的门禁记录写了「全绿」,与实际不符
- **修复(测试工具)**:`build/probe-frontend.js` 的假 DOM 补上 `Image` 与 `replaceChildren` —— app.js 新增的托盘品牌图预载让旧探针在顶层就中断,报出级联假故障,顺带掩盖了第二个缺口
- **流程**:`RELEASE.md` 新增「发版前回归门禁」步骤(core 单测 / CSS 形态门 / jsdom 冒烟 / Android 单测),v0.3.14 的教训成文;`ci.yml` 用例计数注释从 229 更正为 422(忽略 9)

验证:Android `testDebugUnitTest` **36/36**、探针退出码 0、`cargo test` **413** 项(9 忽略)、两个 crate `cargo check --all-targets`、`jsdom-smoke` **162/162**、CSS 形态门 946 条规则、`node --check` ×7、凭据扫描 CLEAN。
