# 2026-11-23 AI 优惠刷新失败

本机 v0.3.71 请求 `https://www.aipromonow.com/rss.xml` 收到 HTTP 410。官网首页与中文优惠列表 `/deals` 返回 HTTP 200。旧版把请求和解析错误都显示为「更新失败」。[官网优惠列表](https://www.aipromonow.com/deals)

桌面端改为每半小时读取公开优惠列表首页。列表 JSON-LD 提供顺序、标题与详情链接，卡片提供摘要。历史发布日期和已读状态按链接迁移。新列表没有发布日期字段。失败时保留上次列表和更新时间，并显示 HTTP 状态或解析错误。官网开发者 API 需要单独申请密钥。[官网 API 文档](https://www.aipromonow.com/developers)

`node build/jsdom-smoke.js` 覆盖正常刷新、旧已读迁移、详情链接、HTTP 410、截断列表、外部链接与刷新重试。本次还用应用的解析器读取了官网返回的实际页面，解析到 24 条带摘要的优惠，首条详情返回 HTTP 200。

发版前复核（v0.3.72）：`/deals` 不经代理与经本地代理都直接返回 200（无跳转），`rss.xml` 仍为 410。官网列表分页，共 411 条 18 页，首页 24 条；JSON-LD 的 `numberOfItems` 等于**当页**条数而非总数，所以「条数与 `numberOfItems` 不一致即拒绝」这条校验不会因分页误报。首页按发布时间倒序（首条 2026-11-22、第 24 条 2026-11-18），因此只取首页等于只展示最近几天新增，更早的历史优惠不再进入桌面端。后端 `redirect::Policy::none()` 不跟随跳转，上游若给 `/deals` 加重定向会显示「优惠来源返回 HTTP 3xx」并保留上次列表。两项取舍记入 `docs/report/acceptance-backlog.md` B13/B14。
