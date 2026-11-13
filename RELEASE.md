# 发布流程

> 桌面交付形态:**只有 NSIS 安装包**(绿色单 exe 已下线,见 PROGRESS「追加调整 12」)。
> 装到 `%LOCALAPPDATA%\Grandettoken`(实测确认,不是 Programs 子目录),**不需要管理员权限**。
> 更新走 GitHub Releases。
>
> **2026-11-12 起,发版走 GitHub Actions**(推版本 tag 即发),本地打包降级为应急。
> 产物由公开、可重放、带完整门禁的流水线生成,而不是某台机器上恰好能过的构建。

## 为什么改回 CI

v0.1.x 时代 CI 发一次要 22 分钟 —— 其中 12 分钟花在 `cargo install tauri-cli`
(tauri-cli 没有官方预编译二进制,`cargo-binstall` / `install-action` / npm 都试过),
所以 2026-10-20 改成本地打包(3 分 11 秒)。代价是**发版不可复现**:
产物取决于本机装的哪个 cargo-tauri、哪份 `target/` 缓存。

2026-11-12 回到 CI,三个前提已经成立:

| 前提 | 做法 |
|---|---|
| tauri-cli 编译可缓存 | `actions/cache` 只存 `~/.cargo/bin/cargo-tauri.exe`,key 与项目依赖锁**解耦**(`cargo-tauri-Windows-v2`);命中后该步 0 秒,冷启动约 11 分钟 |
| Rust 依赖可缓存 | `Swatinem/rust-cache`,`workspaces: app/src-tauri -> target`;命中后 Build 约 2 分钟 |
| 发版前要有闸 | `release.yml` 的 `Pre-release gates`:四道零依赖静态门 + jsdom 冒烟,失败就不打包 |

冷缓存全流程约 25 分钟,热缓存约 8-10 分钟。job 有 `timeout-minutes: 60`,
防止签名步骤在非交互环境等密码而永久挂住(见「应急:本地打包」里的空密码警告)。

## 一次性准备

| 项 | 位置 | 说明 |
|---|---|---|
| 签名私钥 | 本机 `C:\Users\wacil\.tauri\tokenscope.key` + secret `TAURI_SIGNING_PRIVATE_KEY` | ⚠ 丢了就没法再给已装用户推更新 |
| 私钥密码 | secret `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | 密钥未加密,值是空串 |
| 签名公钥 | `tauri.conf.json` 的 `plugins.updater.pubkey` | 公钥丢了可以重新生成,但要连私钥一起换 |
| 仓库可见性 | public | 私有仓库的 release 资源需要鉴权,更新插件拉不到 |
| 本机 Rust 工具链 | 仅本地打包/调试需要 | `stable-x86_64-pc-windows-gnu` + MinGW(`~/mingw64/bin`) |

**私钥必须备份到密码管理器。** 它和公钥是配对的:换了密钥对,老版本装的用户
会因为签名校验失败而永远收不到更新,只能手动重装。

secret 轮换:

```bash
gh secret set TAURI_SIGNING_PRIVATE_KEY < C:/Users/wacil/.tauri/tokenscope.key
gh secret list          # 确认两项都在、更新时间是刚改的
```

## 每次发版

### 1. 版本号**三处**同步

`app/src-tauri/Cargo.toml`、`app/src-tauri/tauri.conf.json`、**`app/src-tauri/Cargo.lock`**
的版本号必须相等:

- 前者是 Rust 里 `env!("CARGO_PKG_VERSION")` 的来源
- `tauri.conf.json` 决定安装包文件名与更新清单里的版本号
- **`Cargo.lock` 最容易漏**。改了 Cargo.toml 不跑 `cargo fetch`,任何带 `--locked`
  的 cargo 调用都会报 `cannot update the lock file`

```bash
cd app/src-tauri && cargo fetch --locked && cd ../..
```

CI 的 `Resolve tag and version` 会断言 tag 与版本号一致,不一致直接红在第一步。

### 2. 提交并推 main

```bash
git add -A && git commit -m "release: vX.Y.Z - 一句话亮点"
git push origin main
```

> ⚠ **先推 main 再打 tag**(2026-11-10 v0.3.31 踩过)。`gh release create` 发现远端
> 没有这个 tag 时会**自动建一个**,取的是**当时的默认分支尖端** —— main 还没推的话,
> tag 就指到上一版的提交,源码归档(zip/tar.gz)拿错快照。走 CI 同样成立:
> tag 指向的 commit 必须在 main 上,否则 GitHub 生成的源码归档与代码不一致。

### 3. 打 tag 并推送(这一步触发发版)

```bash
git tag vX.Y.Z
git push origin refs/tags/vX.Y.Z
```

推送后自动开始:

```
checkout(tag) → 门禁(ipc / silent / size / css / jsdom) → GNU 工具链
  → cargo-tauri(缓存) → MinGW → core 单测 → cargo fetch
  → bundle + 签名 → latest.json → 建 Release → 三件套补传 → verify-release.js
```

看进度:

```bash
gh run list --workflow=release --limit 3
gh run watch <run-id>                  # 实时跟
gh run view <run-id> --log-failed      # 只看失败的那几步
```

### 4. 失败重跑

```bash
gh run rerun <run-id> --failed
```

或整体重走一遍(会覆盖同名资产):

```bash
gh workflow run release.yml -f tag=vX.Y.Z
```

> 重跑是安全的:release 用 `softprops/action-gh-release` + `fail_on_unmatched_files`,
> 同名资产会被替换;之后的「Ensure all three assets」再逐个核对补齐。

### 5. 核验

```bash
export GH_TOKEN=$(printf "protocol=https\nhost=github.com\n\n" | git credential fill | sed -n 's/^password=//p')
node --use-env-proxy build/verify-release.js
```

14 项:更新端点 200 / 版本 / 签名逐字一致 / 安装包可下载且 SHA 一致 / 三件套齐 /
非 draft / 说明完整 / latest 指向本版 / 仓库公开。
CI 里也会跑同一个脚本,所以本地这步是**二次确认**,不是唯一证据。

> Node 24 的 `fetch` 默认不读 `HTTPS_PROXY`;本机设了代理时必须加 `--use-env-proxy`,
> 否则会看到 `fetch failed`(2026-11-12 v0.3.36 踩过)。

## Android 发版（CI，独立 tag）

Android 与桌面共用同一套 GitHub Actions，但入口、资产和更新指针完全独立。
**2026-11-13 起，Android 也走 CI 发版**，本地打包只作应急。

### 1. 打 tag

版本号只从 tag 推导，不再手工改 `build.gradle.kts`：

```bash
git tag android-vX.Y.Z
git push origin refs/tags/android-vX.Y.Z
```

workflow `android-release.yml` 会自动：

```
checkout(tag) → 单测 + assembleRelease（keystore 来自 secrets）
  → 校验证书 SHA-256 / versionName / versionCode
  → 生成 payload → Ed25519 签清单 → 本地回验
  → 更新 android-stable/android-latest.json
  → 建 Android release（--latest=false）→ 匿名下载回验 → 确认桌面 latest 未被抢占
```

`versionCode = major*10000 + minor*100 + patch`（`0.1.13` → `113`）。
必须单调递增；不要复用已发布过的 `versionCode`。

### 2. CI secrets（一次性配置）

以下名称必须存在于仓库 Actions secrets，值只存不打印：

| secret | 内容 |
|---|---|
| `ANDROID_KEYSTORE_BASE64` | `release.p12` 的 base64 |
| `ANDROID_STORE_PASSWORD` | keystore 口令（与 key 口令相同） |
| `ANDROID_KEY_ALIAS` | `grandettoken` |
| `ANDROID_UPDATE_PUBLIC_KEY` | 更新清单 Ed25519 公钥（base64 raw） |
| `ANDROID_CERT_SHA256` | APK 签名证书 SHA-256（小写 hex） |
| `ANDROID_MANIFEST_PRIVATE_KEY` | 更新清单 Ed25519 私钥（PEM 文本） |

`ANDROID_MANIFEST_PRIVATE_KEY` 与已装用户收到的更新签名是同一把 key。
**不要轮换**，否则老版本会因 `UPDATE_KEY_UNKNOWN` 拒绝更新；轮换只能配合一次强制重装。

### 3. 产物与指针

- Android release tag：`android-vX.Y.Z`
- APK 资产名：`Grandettoken-android-vX.Y.Z.apk`
- 更新清单：release `android-stable` 的 `android-latest.json`
- 固定更新地址：
  `https://github.com/wacilimonster-source/Grandettoken/releases/download/android-stable/android-latest.json`
- Android release 必须 `--latest=false`，桌面 `releases/latest` 不能被抢走。
- `android-stable` release **不要删除**；已装 App 只认这个资产。

### 4. 核验

CI 会用 `build/verify-android-release.py` 对**已上传**的清单和 APK 做匿名回验：
签名、schema、applicationId、versionName/versionCode、minSdk、大小、SHA-256、
下载地址逐项比对，并断言 `releases/latest` 仍指向桌面版本。

本地复核同一脚本：

```bash
python build/verify-android-release.py \
  --manifest build/qa/android-vX.Y.Z/android-latest.json \
  --public-key ~/.android/Grandettoken/manifest-public.txt \
  --apk build/qa/android-vX.Y.Z/Grandettoken-android-vX.Y.Z.apk \
  --expected-version-name X.Y.Z \
  --expected-version-code <code> \
  --expected-download-url https://github.com/wacilimonster-source/Grandettoken/releases/download/android-vX.Y.Z/Grandettoken-android-vX.Y.Z.apk
```

### 5. 应急：本地打包（CI 不可用时）

```powershell
.\build\android-release.ps1 -VersionName 0.1.13 -VersionCode 113
```

脚本只产签名 APK，不发布、不签清单。清单仍需
`build/make-android-manifest.mjs` 生成 payload、`build/sign-android-manifest.py`
签名（本地默认走 DPAPI；CI 用 `--private-key-pem`），再手工上传
`android-vX.Y.Z` 和 `android-stable` 两个 release。上传后必须用上面的
`verify-android-release.py` 复核。

## 应急：本地打包

CI 不可用(Actions 故障 / 需要发一个尚未合并的提交 / 需要立即回滚发版)时才用:

```bash
bash build/release-local.sh                       # 约 3-12 分钟,产出 exe + sig
node build/make-latest-json.js --notes-file <说明文件>
```

> ⚠ **空密码必须从 bash export。** 在 PowerShell 里写
> `$env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = ""` 等于**删掉**这个变量(不是设成空串),
> 签名步骤会去等一个永远不来的密码输入,CPU 全程 0 地挂住 —— 实测踩过两次。
>
> 挂住的识别:两次采样 `Get-Process cargo-tauri | Select CPU` 不变,
> 且 `bundle/nsis/` 里只有 `setup.exe`、没有 `.sig`。
> 处理:杀掉 cargo / cargo-tauri,删掉 `app/src-tauri/target/.build-lock`,重跑。

然后手工建 release:

```bash
gh release create vX.Y.Z \
  --title "Grandettoken X.Y.Z — 一句话亮点" \
  --notes-file <说明文件> \
  app/src-tauri/target/release/bundle/nsis/Grandettoken_X.Y.Z_x64-setup.exe \
  app/src-tauri/target/release/bundle/nsis/Grandettoken_X.Y.Z_x64-setup.exe.sig \
  app/src-tauri/target/release/bundle/nsis/latest.json
```

**⚠ 上传完必须核一遍三件套齐不齐**(2026-10-20 踩过:`.sig` 上传后"消失",
release 只挂上 2 个文件,已装用户的自动更新直接断):

```bash
gh release view vX.Y.Z --json assets --jq '.assets[].name'
# 必须看到 3 个:setup.exe / setup.exe.sig / latest.json
```

`gh` 没登录时,可以直接用它已经存在 Windows 凭据管理器里的凭据(不落盘、不回显):

```bash
export GH_TOKEN=$(printf "protocol=https\nhost=github.com\n\n" | git credential fill | sed -n 's/^password=//p')
```

更新端点固定是
`https://github.com/wacilimonster-source/Grandettoken/releases/latest/download/latest.json`
—— 这个 URL 永远指向最新 release,不用改配置。

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
