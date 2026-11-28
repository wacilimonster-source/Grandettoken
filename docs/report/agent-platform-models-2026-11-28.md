# Agent Platform 可见模型不全 — 排查与修复（2026-11-28）

## 结论

`agent_platform` 渠道的可见模型清单此前只来自 `AGENT_PLATFORM_PRESET_MODELS` 的 5 个硬编码预置模型；
`fetch_channel_models` 从未向上游请求模型列表，因此清单不可能反映账号实际可用的全部模型。

修复后：

1. 内置候选清单扩展为 Agent Platform 官方目录中的 16 个 Gemini 对话模型；
2. 对官方 Vertex OpenAI 兼容端点，用 `publishers/google/models/{model}:countTokens` 逐个校验候选模型
   （200=可用，404=不存在/无项目权限，其余按未知保留），按账号/项目过滤后再展示；
3. 自建 / 转售的 Agent Platform 兼容端点优先读取 OpenAI `/models`，失败时退回内置全量清单；
4. 手动模型仍然合并且优先保留。

## 为什么不能直接拉 `/models`

用真实 Agent Platform 密钥实测（仅记录状态，不记录密钥）：

| 请求 | 结果 |
|---|---|
| `GET {base}/models` | 404（Google 前端 HTML，不是 API JSON） |
| `GET /v1/projects/{p}/locations/global/models` | 401 `API keys are not supported by this API` |
| `GET /v1beta1/publishers/google/models` | 401，同样要求 OAuth |
| 官方 express mode API 参考 | 只有 `generateContent` / `streamGenerateContent` / `countTokens`，没有 `models.list` |

即：API Key 认证下没有可用的模型列表接口；`models.list` 需要 OAuth 主体。
因此采用 `countTokens` 探测：它不生成 token，能可靠区分「模型存在且项目可用」（200）与
「不存在或项目无权」（404）。

## 实测（2026-11-28，真实 project / 系统代理）

16 个候选 → 13 个可用、3 个 404（`gemini-2.5-pro`、`gemini-2.5-flash`、`gemini-2.5-flash-lite`），
无「未知」，耗时约 9.5s。

可用清单（按内置顺序）：

```
google/gemini-3.8-flash
google/gemini-3.5-flash-lite
google/gemini-3.1-flash-lite
google/gemini-4-argon
google/gemini-4-hydrogen
google/gemini-4-carbon
google/gemini-4-neon
google/gemini-3.7-flash
google/gemini-3.6-flash
google/gemini-3.5-flash
google/gemini-3.1-pro-preview
google/gemini-3.1-pro-preview-customtools
google/gemini-3-flash-preview
```

## 代码与验证

- `app/core/src/gateway/http/agent_models.rs`：候选清单探测、官方端点路径推导、自建端点 `/models` 回退；
- `app/core/src/gateway_proto/sources.rs`：内置候选清单扩展到 16 个，并新增清单完整性单测；
- `app/core/src/gateway/http/tests.rs`：探测状态合并、404 回退全量清单、自建端点 `/models` 直取；
- `app/src/gateway-view.js`：上架抽屉的手动模型提示改为「已内置 Agent Platform 全量 Gemini 模型清单」。

验证：`cargo test --locked`（core 733 passed / 10 ignored + sync_fixtures 6 passed）、
双端 `cargo check --all-targets --locked`、全量门禁串行通过。
