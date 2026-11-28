# 发布流程

> 桌面交付形态:**只有 NSIS 安装包**(绿色单 exe 已下线,见 `docs/report/progress-archive-2026-11-22.md`)。
> 装到 `%LOCALAPPDATA%\Grandettoken`,**不需要管理员权限**;更新走 GitHub Releases。
>
> **2026-11-28 起,桌面与 Android 统一改为本机发版**,并接入 NW 共享发版目录(`..\_release`)。
> 构建依赖、归档目录与轮转工具全 NW 共用,GitHub Actions 只保留 `ci.yml` 与 `text-encoding.yml` 做推送门禁。

## 共用依赖与仓库分工

共享配置见 [`../_release/README.md`](../_release/README.md) 与 [`../_release/toolchains.json`](../_release/toolchains.json);本项目的渠道声明在 [`tool/release.json`](tool/release.json):

| 项目 | 登记名称 | 共用内容 | 项目自留内容 |
|---|---|---|---|
| 桌面端(`desktop`) | `tauri2-gnu` | `rustup` GNU 工具链、`CARGO_HOME`、`MinGW`、`cargo-tauri` | `app/src-tauri/target`、签名私钥、`latest.json` |
| Android(`android`) | `android-gradle` | `G:\Android`(SDK + Build Tools 35.0.0)、JDK 17、`C:\Users\wacil\.gradle` | `android/` 构建缓存、keystore、Ed25519 私钥 |

- 不在项目内复制 Rust/MinGW/Android SDK 或私建 `.gradle` 家目录。
- 升级共享工具链时,在 `..\_release\toolchains.json` 新增名称,本项目验证一版后再删除旧版本。
- 发版入口(`build/release-desktop.ps1`、`build/release-android.ps1`)会锁定 `toolchains.json` 登记的路径与版本,不依赖系统默认 `PATH`。

## 一次性准备

| 项 | 位置 | 说明 |
|---|---|---|
| 桌面签名私钥 | `C:\Users\wacil\.tauri\tokenscope.key` | 未加密密钥,配套公钥在 `app/src-tauri/tauri.conf.json` 的 `plugins.updater.pubkey`。⚠ 丢了就无法给已装用户推更新 |
| Android APK 签名仓 | `C:\Users\wacil\.android\Grandettoken\release.p12` | 别名 `grandettoken`;口令密文 `release-password.dpapi`(当前 Windows 用户 DPAPI,也可设 `GRANDETTOKEN_ANDROID_STORE_PASSWORD` 或交互输入) |
| Android 清单私钥 | `C:\Users\wacil\.android\Grandettoken\manifest-private.dpapi` | Ed25519 私钥(当前 Windows 用户 DPAPI),配套公钥与证书指纹在 `android/release-public/` |
| GitHub 凭据 | `gh auth login` 或 `GH_TOKEN` | 脚本未设 `GH_TOKEN` 且 `gh` 未登录时自动从 `git credential fill` 读取,不落盘不回显 |
| 仓库可见性 | public | 私有仓库的 Release 资源需要鉴权,应用内置更新插件拉不到 |

## 历史记录与清理机制

为了避免本地归档、仓库文件和 GitHub Releases 无限膨胀,发版历史统一按以下上限保留:

| 位置 | 保留策略 | 执行方式 |
|---|---|---|
| 仓库文件 | 不保留逐版本发版说明(`build/release-notes-*.md` / `updater-notes-*.md` 已清除并由 `check-tracked-artifacts.js` 拦截) | 发版说明统一写在发版提交正文和 GitHub Release 正文里 |
| 构建/核验日志 | 不入库;临时产出放 `build/qa/`(已 gitignore),发版完成即可删 | — |
| NW 本地归档 | `..\_release\artifacts\Grandettoken\<channel>\<version>\` 每个渠道保留最近 **3** 个版本(`keep.local = 3`),以及已验收版本和比它更新的版本 | 验收后预览并执行 `rotate --apply <PLAN_SHA256>` |
| GitHub Releases | 每个渠道保留最近 **3** 个已发布版本(`keep.remote = 3`);GitHub `Latest`、`android-stable` 固定指针、草稿和不认识的 tag 始终保留 | 验收后预览并执行 `prune-remote --apply <PLAN_SHA256>`(默认保留 git tag;加 `-WithTags` 才连 tag 一起删) |

- 归档目录写入后只读,重跑时逐字节对账,内容不一致立即报错。
- 破坏性清理全部是两段式:**先预览输出待删清单与 `PLAN_SHA256`,核对无误后再带摘要执行 `--apply`**。

## 桌面端发版

### 1. 同步版本号(三处)

`app/src-tauri/Cargo.toml`、`app/src-tauri/tauri.conf.json` 与 `app/src-tauri/Cargo.lock` 必须相等:

```bash
cd app/src-tauri && cargo fetch --offline && cd ../..
git diff app/src-tauri/Cargo.lock                        # 正常只应看到本包 tokenscope 的 version 行
cd app/src-tauri && cargo fetch --locked && cd ../..
```

> 第一步不能直接 `--locked`:刚改完 `Cargo.toml` 版本号时 lock 必须更新本包这一行,`--offline` 用本机缓存刷新后再用 `--locked` 复核。

### 2. 更新验收清单、跑全量门禁并推送 `main`

1. 把本版需要真机/安装版验收的条目写入 [`docs/report/acceptance-backlog.md`](docs/report/acceptance-backlog.md),并更新 `PROGRESS.md` 文末。
2. 涉及桌面界面、窗口几何或 WebView2 行为的改动,先关闭运行中的 `Grandettoken`,执行一键原生 CDP 回归:
   ```powershell
   powershell -NoProfile -ExecutionPolicy Bypass -File build/run-native-checks.ps1
   ```
3. 提交并推送 `main`(发版提交正文须至少 40 字符且含 20 个以上中文字符,原样写入 `latest.json.notes` 和 Release 正文):
   ```bash
   git add -A && git commit -m "release: vX.Y.Z - 一句话亮点" -m "- 本版变更一" -m "- 本版变更二"
   git push origin main
   ```

> ⚠ **必须先推 `main` 再发版**(`HEAD == origin/main` 且工作树干净),脚本硬拦截未推送提交,避免 tag 与源码归档错位。

### 3. 一键本机构建、签名、归档与发布

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File build/release-desktop.ps1 -Stage Release
```

也可分段执行(`-Stage Preflight` → `-Stage Build` → `-Stage Publish`):

1. `Preflight`:校验工作树干净、`HEAD == origin/main`、tag 未被占用、提交正文达标、GitHub `Latest` 未超前、工具链与私钥就绪,串行跑完 [`build/run-gates.ps1`](build/run-gates.ps1)(对应 `AGENTS.md` §4.1 全部门禁)并写当前提交的通过标记。
2. `Build`:设置 `TAURI_SIGNING_PRIVATE_KEY` 与 `CI=true`(使 `cargo tauri build` 回落空密码,避免 PowerShell `$env:X = ''` 删变量导致挂死),调 `build/build.ps1 bundle` 编 NSIS 安装包、打本地 tag `vX.Y.Z`、生成 `latest.json`、用配置公钥离线验签(`verify-signature.js --local`),然后把 `setup.exe`、`.sig`、`latest.json`、`release-notes.md` 归档到 `..\_release\artifacts\Grandettoken\desktop\X.Y.Z\`。
3. `Publish`:推送 `vX.Y.Z` tag 到 `origin`,建草稿 Release 并上传三件套,逐件校验远端资产状态/字节数/SHA-256(异常时在草稿态幂等补传),转正并设为 `Latest`,最后跑 `node --use-env-proxy build/verify-release.js` 做端到端核验。

> 若手动排查签名挂死:两次采样 `Get-Process cargo-tauri | Select CPU` 不变且 `bundle/nsis/` 只有 `setup.exe` 无 `.sig` 即为等密码挂住;杀掉进程并删除 `app/src-tauri/target/.build-lock` 后重跑。

### 4. 覆盖升级验收与清理历史

1. 用旧版本安装包的「检查更新」下载新版覆盖安装,确认应用正常升级与核心功能通过,并在 [`docs/report/acceptance-backlog.md`](docs/report/acceptance-backlog.md) 标为完成。
2. 记录验收并预览清理计划:
   ```powershell
   powershell -NoProfile -ExecutionPolicy Bypass -File build/release-desktop.ps1 -Stage Accept
   ```
3. 核对输出的文件列表与 Release 列表后,按返回的摘要执行清理(若也要顺手删旧远程 tag 则加 `-WithTags`):
   ```powershell
   powershell -NoProfile -ExecutionPolicy Bypass -File build/release-desktop.ps1 -Stage Prune -ApplyLocal <local_plan_sha256> -ApplyRemote <remote_plan_sha256>
   ```

## Android 发版

Android 与桌面共用同一仓库与 `..\_release\release_store.py`,但渠道(`android`)、tag(`android-vX.Y.Z`)、APK(`Grandettoken-android-vX.Y.Z.apk`)与更新指针(`android-stable/android-latest.json`)完全独立,**绝不抢占桌面 `Latest`**。

`versionCode = major*10000 + minor*100 + patch`(如 `0.1.15` → `10115`),随版本号单调递增。

1. 提交并推送 `main`(提交正文需 >= 20 字符)。
2. 本机跑单测、构建签名 APK、签更新清单、归档并发布:
   ```powershell
   powershell -NoProfile -ExecutionPolicy Bypass -File build/release-android.ps1 -Version 0.1.15 -Stage Release
   ```
   - `Build`:校验 `HEAD == origin/main` 与版本号单调性,调共享 `android-gradle` 工具链运行 `:app:testDebugUnitTest :app:assembleRelease`,用 `aapt`/`apksigner` 校验证书指纹与包信息,生成并签名 `android-latest.json`,本地运行 `verify-android-release.py` 后归档至 `..\_release\artifacts\Grandettoken\android\0.1.15\`。
   - `Publish`:推送 `android-v0.1.15` tag,创建非 `Latest` Release 上传 APK,更新固定指针 `android-stable` 的 `android-latest.json`,匿名下载回验并用 `verify-android-release.py` 校验,同时确认桌面 `Latest` 未被抢占。
3. 旧版 App 实测覆盖升级后执行验收与清理预览:
   ```powershell
   powershell -NoProfile -ExecutionPolicy Bypass -File build/release-android.ps1 -Version 0.1.15 -Stage Accept
   powershell -NoProfile -ExecutionPolicy Bypass -File build/release-android.ps1 -Stage Prune -ApplyLocal <local_plan_sha256> -ApplyRemote <remote_plan_sha256>
   ```

- `android-stable` Release **不要删除**,已装 App 只认该固定资产(`keep.remote` 逻辑已将其列为 `pinned`)。

## 签名材料备份与恢复演练

本机 `manifest-private.dpapi` 与 `release-password.dpapi` 受当前 Windows 用户 DPAPI 保护,换机、重装系统或换 Windows 账户后**无法解密**。以下三样私密材料必须存进密码管理器或离线加密介质(**不经 DPAPI**),严禁进仓库/日志/聊天:

| 材料 | 丢了的后果 |
|---|---|
| `tokenscope.key`(桌面) | 已装桌面用户收不到更新 |
| `release.p12` + 口令 + 别名 `grandettoken` | 新 APK 证书变化,已装用户无法覆盖升级(同步组清空) |
| Android 清单 Ed25519 私钥(明文 PEM) | 已装 App 以 `UPDATE_KEY_UNKNOWN` 拒收新清单 |

**恢复演练**(换机前或改签名流程后,在另一台机器/Windows Sandbox 里只用备份执行):

1. APK 证书:`keytool -list -v -keystore release.p12 -storetype PKCS12 -alias grandettoken`,SHA-256 指纹(去冒号小写)须与 `android/release-public/certificate-sha256.txt` 一致。
2. 清单私钥:`python build/sign-android-manifest.py --payload test-payload.json --out test-signed.json --private-key-pem <备份的 PEM> --public-key android/release-public/manifest-public.txt`,输出 `signed ...` 即通过。
3. 桌面私钥:`cargo tauri signer sign -f <备份的 tokenscope.key> -p "" <任意测试文件>` 生成 `.sig` 后,运行 `node build/verify-signature.js --local <测试文件> --sig <测试文件>.sig`,输出 `RESULT: PASS` 即通过(`--self-test` 可随时自检)。
4. 结果记入 `docs/report/acceptance-backlog.md`:日期、通过与否、指纹前 8 位(**不记任何口令**)。演练后清理演练机临时文件。

## 用户数据与卸载

- 数据库在 `%APPDATA%\com.wacil.tokenscope\tokenscope.db`(目录名沿用旧 bundle id),密钥在 Windows 凭据管理器,**都不在安装目录里**,升级、重装、覆盖安装均不丢数据。
- 卸载入口:开始菜单 → Grandettoken → Uninstall(或 `%LOCALAPPDATA%\Grandettoken\uninstall.exe`),卸载时自动清理开机自启项与受管代理网关注入块。
