# 发布流程

> 交付形态:**只有 NSIS 安装包**(绿色单 exe 已下线,见 PROGRESS「追加调整 12」)。
>
> **发版改成本地打包(2026-10-20 起)。**
>
> 原因:CI 发版一次要 **22 分钟**,其中 12 分钟花在 `cargo install tauri-cli` ——
> 而 tauri-cli **没有官方预编译二进制**(实测:`tauri-apps/tauri` 的 release assets 为 0、
> `cargo-quickinstall` 最近 100 个 release 里命中 0、`@tauri-apps/cli-win32-x64-gnu`
> 在 npm 上是 404),所以每次都得从源码编。本机已经有编好的 `cargo-tauri.exe`,
> 且 `target/` 有增量缓存 —— **本地打包比 CI 快得多**。
>
> CI 的 `release.yml` 保留(手工 `workflow_dispatch` 仍可用作应急),但**不再靠推 tag 触发**。

产物是 **NSIS 安装包**(per-user,装到 `%LOCALAPPDATA%\Grandettoken`(实测确认,不是 Programs 子目录),
**不需要管理员权限**)。更新走 GitHub Releases。

## 一次性准备

| 项 | 位置 | 说明 |
|---|---|---|
| 签名私钥 | `C:\Users\wacil\.tauri\tokenscope.key` | ⚠ 丢了就没法再给已装用户推更新 |
| 签名公钥 | `tauri.conf.json` 的 `plugins.updater.pubkey` | 公钥丢了可以重新生成,但要连私钥一起换 |
| Tauri CLI | `cargo install tauri-cli` | 已装(本机 `~/.cargo/bin/cargo-tauri.exe`) |
| Rust 工具链 | `stable-x86_64-pc-windows-gnu` + MinGW(`~/mingw64/bin`) | 由 `build.ps1` 自己钉住 |
| GitHub secrets | 仅 CI 应急链路需要 | 本地打包**不需要** secrets |

**私钥必须备份到密码管理器。** 它和公钥是配对的:换了密钥对,老版本装的用户
会因为签名校验失败而永远收不到更新,只能手动重装。

**仓库必须保持 public。** 私有仓库的 release 资源需要鉴权,更新插件拉不到 —— 这是硬约束。

## 每次发版(本地打包)

### 1. 版本号**三处**同步

`app/src-tauri/Cargo.toml`、`app/src-tauri/tauri.conf.json`、**`app/src-tauri/Cargo.lock`**
的版本号必须相等:

- 前者是 Rust 里 `env!("CARGO_PKG_VERSION")` 的来源
- `tauri.conf.json` 决定安装包文件名与更新清单里的版本号
- **`Cargo.lock` 最容易漏**。改了 Cargo.toml 不跑 `cargo fetch`,任何带 `--locked`
  的 cargo 调用都会报 `cannot update the lock file`

```bash
cd app/src-tauri && cargo fetch && cd ../..
```

`build/make-latest-json.js` 会断言这三处一致,漏了在本地就红。

### 2. 本地打包(一条命令)

```bash
bash build/release-local.sh
```

它做的事:在 **bash 里** export 签名变量(见下方警告)、钉住 GNU 工具链、调
`build/build.ps1 bundle`。耗时约 **8-12 分钟**(本机增量缓存,不含 CI 那 12 分钟的
`cargo install`)。

> ⚠ **空密码必须从 bash export。** 在 PowerShell 里写
> `$env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = ""` 等于**删掉**这个变量(不是设成空串),
> 签名步骤会去等一个永远不来的密码输入,CPU 全程 0 地挂住 —— 实测踩过两次。
>
> 挂住的识别:两次采样 `Get-Process cargo-tauri | Select CPU` 不变,
> 且 `bundle/nsis/` 里只有 `setup.exe`、没有 `.sig`。
> 处理:杀掉 cargo / cargo-tauri,删掉 `app/src-tauri/target/.build-lock`,重跑。

产出(在 `app/src-tauri/target/release/bundle/nsis/`):

```
Grandettoken_<版本>_x64-setup.exe       # 安装包
Grandettoken_<版本>_x64-setup.exe.sig   # 签名,自动更新校验用
```

### 3. 生成更新清单

```bash
node build/make-latest-json.js "更新说明写在这里"
```

产出 `latest.json`,里面 `signature` 是 `.sig` 的**文件内容**(不是路径),
`url` 指向该 release 的 setup.exe。脚本会顺带校验三处版本号一致、`.sig` 存在。

### 4. 上传到 GitHub Release

```bash
gh auth login            # 只第一次需要

gh release create v0.2.0 \
  --title "Grandettoken 0.2.0 — 一句话亮点" \
  --notes "更新说明" \
  app/src-tauri/target/release/bundle/nsis/Grandettoken_0.2.0_x64-setup.exe \
  app/src-tauri/target/release/bundle/nsis/Grandettoken_0.2.0_x64-setup.exe.sig \
  app/src-tauri/target/release/bundle/nsis/latest.json
```

**⚠ 上传完必须核一遍三件套齐不齐**(2026-10-20 踩过:`.sig` 上传后"消失",
release 只挂上 2 个文件,已装用户的自动更新直接断):

```bash
gh release view v0.2.0 --json assets --jq '.assets[].name'
# 必须看到 3 个:setup.exe / setup.exe.sig / latest.json
```

少了就用 `--clobber` 补传:

```bash
gh release upload v0.2.0 <缺的那个文件> --clobber
```

`gh` 没登录时,可以直接用它已经存在 Windows 凭据管理器里的凭据
(不落盘、不回显):

```bash
export GH_TOKEN=$(printf "protocol=https\nhost=github.com\n\n" | git credential fill | sed -n 's/^password=//p')
```

更新端点固定是
`https://github.com/wacilimonster-source/Grandettoken/releases/latest/download/latest.json`
—— 这个 URL 永远指向最新 release,不用改配置。

### 5. 推送代码与 tag

```bash
git add -A && git commit -m "release: vX.Y.Z"
git tag vX.Y.Z
git push && git push --tags
```

> 推 tag **不再触发**自动发版(见文件顶部说明)。CI 的 `release.yml` 只剩手工
> `workflow_dispatch` 应急入口。

### 6. 验证

装了旧版本的机器上启动应用 → 底栏出现「有新版本 X.Y.Z」→ 点进去 →
「立即更新」→ 下载 → 自动退出并静默安装 → 重启后版本号变了。

也可以在本地先跑核验脚本(只读,不写任何东西):

```bash
node build/verify-release.js
```

## 用户数据

数据库在 `%APPDATA%\com.wacil.tokenscope\tokenscope.db`(目录名是 bundle id,
产品改名后不变),密钥在 Windows 凭据管理器,
**都不在安装目录里**,所以升级、重装、覆盖安装都不丢数据。卸载默认也不删。

## 卸载

开始菜单 → Grandettoken → Uninstall(或 `%LOCALAPPDATA%\Grandettoken\uninstall.exe`)。
卸载时应该清掉开机自启注册表键(`HKCU\...\Run` 里的 Grandettoken),
否则会残留一个指向已删除 exe 的自启项。

> 签名私钥在 `C:\Users\wacil\.tauri\tokenscope.key`(文件名沿用旧产品名,内容与
> `tauri.conf.json` 里的公钥配套;产品改名不影响这一对密钥)。
