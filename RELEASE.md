# 发布流程

产物是 **NSIS 安装包**(per-user,装到 `%LOCALAPPDATA%\Programs\TokenScope`,
**不需要管理员权限**)。更新走 GitHub Releases。

## 一次性准备(已完成,别重复做)

| 项 | 位置 | 说明 |
|---|---|---|
| 签名私钥 | `C:\Users\wacil\.tauri\tokenscope.key` | ⚠ 丢了就没法再给已装用户推更新 |
| 签名公钥 | `tauri.conf.json` 的 `plugins.updater.pubkey` | 公钥丢了可以重新生成,但要连私钥一起换 |
| Tauri CLI | `cargo install tauri-cli` | 已装 |

**私钥必须备份到密码管理器。** 它和公钥是配对的:换了密钥对,老版本装的用户
会因为签名校验失败而永远收不到更新,只能手动重装。

## 每次发版

### 1. 版本号两处同步

`app/src-tauri/Cargo.toml` 的 `version` 与 `app/src-tauri/tauri.conf.json` 的
`version` **必须相等**。前者是 Rust 里 `env!("CARGO_PKG_VERSION")` 的来源,
后者决定安装包文件名和更新清单里的版本号 —— 不一致会出现"装完检测到自己是新版"
或者永远检测不到。`build/make-latest-json.js` 会断言这一步。

### 2. 提交并打 tag

```bash
git add -A && git commit -m "release: v0.2.0"
git tag v0.2.0
git push && git push --tags
```

### 3. 构建(私钥必须在环境变量里)

`.env` 文件**不起作用**,必须是真的环境变量:

```bash
cd app/src-tauri
export TAURI_SIGNING_PRIVATE_KEY="C:\\Users\\wacil\\.tauri\\tokenscope.key"
# 必须显式给空密码!密钥未加密,但不设这个变量时 Tauri 会去等交互输入密码 ——
# 在非交互环境(脚本 / AI 会话)里会永久挂住,日志最后一行停在
# "Info Decrypting updater signing key, expect a prompt for password" 且 CPU 全程 0。
# 实测踩过两次(第一次还误判成网络卡住)。
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD=""
cargo tauri build
```

产出(在 `app/src-tauri/target/release/bundle/nsis/`):

```
TokenScope_<版本>_x64-setup.exe       # 安装包,人手动下载
TokenScope_<版本>_x64-setup.exe.sig   # 签名,自动更新校验用
```

### 4. 生成更新清单

```bash
node build/make-latest-json.js "更新说明写在这里"
```

产出 `latest.json`,里面 `signature` 是 `.sig` 的**文件内容**(不是路径),
`url` 指向该 release 的 setup.exe。脚本会顺带校验两处版本号一致、`.sig` 存在。

### 5. 上传到 GitHub Release

```bash
gh auth login            # 只第一次需要
gh release create v0.2.0 \
  --title "TokenScope v0.2.0" \
  --notes "更新说明" \
  target/release/bundle/nsis/TokenScope_0.2.0_x64-setup.exe \
  target/release/bundle/nsis/TokenScope_0.2.0_x64-setup.exe.sig \
  target/release/bundle/nsis/latest.json
```

也可以用网页上传。**三个文件都要传**,少一个更新链路就断。

更新端点固定是
`https://github.com/wacilimonster-source/Grandettoken/releases/latest/download/latest.json`
—— 这个 URL 永远指向最新 release,不用改配置。

> ⚠ 仓库必须保持 **public**。私有仓库的 release 资源需要鉴权,更新插件拉不到。

### 6. 验证

装了旧版本的机器上启动应用 → 底栏出现「有新版本 X.Y.Z」→ 点进去 →
「立即更新」→ 下载 → 自动退出并静默安装 → 重启后版本号变了。

## 用户数据

数据库在 `%APPDATA%\com.wacil.tokenscope\tokenscope.db`,密钥在 Windows 凭据管理器,
**都不在安装目录里**,所以升级、重装、覆盖安装都不丢数据。卸载默认也不删。

## 卸载

开始菜单 → TokenScope → Uninstall。卸载时应该清掉开机自启注册表键
(`HKCU\...\Run` 里的 TokenScope),否则会残留一个指向已删除 exe 的自启项。
