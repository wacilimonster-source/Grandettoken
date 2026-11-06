# v0.3.17 额度恢复回归记录

代码提交：`2e0033a`。平台：Windows x64，Rust GNU 工具链，Node v24。

| 检查 | 结果 |
|---|---|
| core `cargo test --locked` | 417 单元测试 + 6 契约夹具测试通过；8 个真机测试和 1 个文档测试按设计忽略 |
| core `cargo check --all-targets --locked` | 通过，无警告 |
| 桌面端 `build/build.ps1 check` | 通过，包含 `cargo check --all-targets` 与静态 WebView2 加载器准备 |
| 前端 `jsdom-smoke.js` | 169/169 通过 |
| CSS 形态守卫 | 通过，7 个整块容器、946 条规则 |
| JavaScript 语法 | app.js、jsdom-smoke 与 5 个发布/验证脚本通过 |
| 凭据扫描 | 339 个跟踪文件 CLEAN，9 个既有测试夹具按规则跳过 |
| diff 空白检查 | 通过 |

新增核心回归覆盖：共享尝试计数、503 后使用备用入口成功、429 的等待时间、手动刷新不绕过限流、重试上限与时间预算、业务错误退出密集重试、持久化窗口和到期信息、换账户与令牌轮换、删除凭据、损坏缓存、失败不覆盖成功时间、首次无历史仍继续重试、跨周期提示。

新增前端回归覆盖：面板和紧凑条继续显示额度、自动胶囊继续选择历史渠道、托盘数字保留、吸附条显示陈旧标记、重试耗尽保留额度、恢复后解除提示。保留了原有 Codex 双额度与明确鉴权失败回归。

未操作用户的真实平台账户；外网平台凭据用例仍忽略。未执行安装包覆盖安装或真实桌面视觉交互验证。Android 源码没有改动，本次为 Windows 桌面版发布。

首次运行既有打包脚本时，Windows PowerShell 继承宿主 PowerShell 模块路径，无法自动加载 `Get-FileHash`。仅为本次打包进程指定 Windows PowerShell 原生模块路径后重新执行，未修改系统配置或仓库发布脚本。

## 发布核验

- [v0.3.17](https://github.com/wacilimonster-source/Grandettoken/releases/tag/v0.3.17) 已公开发布，指向代码提交 `2e0033a`；先上传草稿三件套，再设为 Latest。
- `Grandettoken.exe` 产品与文件版本均为 `0.3.17`；构建脚本确认重新链接且导入表无 `WebView2Loader.dll` 依赖。GNU 链接器有既有静态库 `.drectve` 警告，构建退出码为 0。
- 安装包 `Grandettoken_0.3.17_x64-setup.exe`：5,559,301 字节，SHA-256 `b4cc9a3f94cda0298900818906290d13fbf978465fae5232ea26ba9f32b5fd35`。
- 使用应用配置的公开更新密钥独立验证 Ed25519 签名、密钥 ID 与可信注释；修改安装包一个字节后的签名验证按预期失败。
- `node build/verify-release.js` 全部 PASS：公开下载 HTTP 200、远端与本地哈希及签名一致、安装包/签名/清单齐全、说明中文完整、非草稿/预发布、仓库公开、固定自动更新端点指向 `0.3.17`。
- GitHub [CI](https://github.com/wacilimonster-source/Grandettoken/actions/runs/42204096939) 为额外复核；发布时仍在执行环境准备。本次发布门禁依据上述已通过的本地测试与产物核验。
