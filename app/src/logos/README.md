# 渠道官方图标

界面里渠道行的图标。有官方标的渠道用图片(本目录),其余渠道仍是 CSS 画的
字母方块(见 `app.js` 的 `iconHtml`)。

| 文件 | 渠道 | 来源 | 抓取日期 |
|---|---|---|---|
| `opencode.svg` | OpenCode Go | https://opencode.ai/favicon.svg | 2026-09-17 |
| `deepseek.png` | DeepSeek | https://www.deepseek.com/favicon.ico(225×225 帧转 PNG) | 2026-09-17 |
| `trae.svg` | Trae | 本机已安装的 TRAE SOLO CN 客户端自带素材 `resources/app/out/media/trae-logo.svg` | 2026-09-17 |
| `workbuddy.png` | WorkBuddy | 本机已安装的 WorkBuddy 客户端 `Assets/Square44x44Logo.targetsize-256.png`(256×256) | 2026-09-17 |

说明:

- 这些是各自服务的官方商标,在这里只用于**标识对应渠道**(指明性使用),
  不表示任何隶属或背书;版权归各自所有者。
- 素材以文件形式随应用内嵌发布,不联网加载 —— 前端 CSP 是
  `img-src 'self' data:`,只允许本地资源与 data URI。
- OpenCode 的标自带深色底(`#131010`),在深色界面里呈现为白色方框;
  DeepSeek 的鲸鱼是品牌蓝 `#4d6bfe`、透明底,直接放在界面底色上
  (与列表里 DeepSeek 的主题色一致)。两者都不要再加渠道色方块。
- Trae / WorkBuddy 的图标**直接取自本机安装的客户端**,和用户桌面/任务栏里
  看到的是同一个标:Trae 是深底(`#1A1B1D`)+ 荧光绿几何标,WorkBuddy 是
  绿色圆角块 + 白色标。两者都是透明底,放在 `.ico` 容器里即可。
- 未配置密钥 / 取数失败时,图标统一压暗去色(`.ico.logo.off`),与字母方块
  变灰的语义保持一致。
