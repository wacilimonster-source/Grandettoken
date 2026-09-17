# 交付方式改为安装包 + 检查更新(设计稿 v1)

> 状态:**待确认**。本文件只做设计,尚未改任何实现代码。
> 关联:`DESIGN.md`(产品总设计)、`PROGRESS.md`「单 exe 交付」章节。

---

## 1. 这次要解决什么

原来的产物约束是「**单 exe、免安装、绿色运行**」(`PROGRESS.md` 用户确认项)。
为了守住这条,项目做了两件代价很高的事:

1. **`build/webview2-static/` 那套符号垫片** —— GNU 工具链下 `webview2-com-sys`
   动态导入 `WebView2Loader.dll`,为了让 exe 能一个人跑,硬把 MSVC 的
   `WebView2LoaderStatic.lib` 重打成 GNU 归档,还手搓了 MSVC CRT 垫片
   (`__security_cookie`、`_Init_thread_*`、`operator new/delete`)和汇编别名。
   它现在能用,但属于"编译器边界上的手工活",每次工具链升级都要重新验证。
2. **没有稳定安装路径** —— 自启注册表只能靠"每次启动重写 exe 路径"补救
   (`lib.rs` setup),快捷方式、卸载项、开始菜单全都没有。

改成安装包后,这两件事**一起消失**:DLL 由安装包分发,路径由安装包固定。

顺带补上一个现在完全缺失的能力:**应用自己能知道自己有新版本**。

---

## 2. 交付形态(决策 D1)

**推荐:NSIS 安装包为唯一交付形态,绿色单 exe 下线。**

| | 绿色单 exe(现状) | NSIS 安装包(推荐) |
|---|---|---|
| 安装 | 无 | 双击一次,per-user |
| 管理员权限 | 不需要 | **不需要**(`currentUser`) |
| WebView2Loader | 靠符号垫片硬链 | 安装包带上,160 KB |
| 稳定路径 | 无(用户随便放) | `%LOCALAPPDATA%\Programs\TokenScope` |
| 开始菜单 / 卸载项 | 无 | 有 |
| 自动更新 | 需要自己替换 exe(自启、快捷方式都对不上) | 官方链路,一键完成 |
| 自制复杂度 | 高(垫片 + build.rs 特判) | 低 |

**要不要保留 zip 绿色版?** 建议**不保留**。理由:两条交付线 = 两套更新逻辑 +
双倍回归量;而绿色版唯一的优势"免安装",安装包已经用「per-user、无管理员、
不写系统目录」覆盖了。真要保留,也只做"提示有新版本 + 手动下载",不自动安装。

> ⚠ 这一条会覆盖 `PROGRESS.md` 里用户确认过的「单 exe、免安装、绿色运行」约束,
> 需要你点头。

---

## 3. 安装包设计(NSIS)

```jsonc
// tauri.conf.json
"bundle": {
  "active": true,
  "targets": ["nsis"],
  "createUpdaterArtifacts": true,        // 新增:产出签名,更新必需
  "windows": {
    "nsis": {
      "installMode": "currentUser",      // 默认即为 currentUser,显式写出来
      "languages": ["SimpChinese"],
      "displayLanguageSelector": false
    },
    "webviewInstallMode": { "type": "embedBootstrapper" }
  }
}
```

| 项 | 取值 | 理由 |
|---|---|---|
| 安装位置 | `%LOCALAPPDATA%\Programs\TokenScope` | per-user 默认,**不需要管理员** |
| `installMode` | `currentUser` | 保持"免管理员"这条硬约束 |
| `webviewInstallMode` | `embedBootstrapper` | 默认 `downloadBootstrapper` 安装时要联网;内嵌只多 1.8 MB,换来"断网也能装" |
| `createUpdaterArtifacts` | `true` | 不打开就没有 `.sig`,更新链路不成立 |
| 卸载 | 开始菜单卸载项 | 卸载时清理 HKCU Run 键(见第 9 节) |

产物:`app/src-tauri/target/release/bundle/nsis/TokenScope_0.2.0_x64-setup.exe`

---

## 4. WebView2Loader 垫片去留(决策 D2)

| 方案 | 做法 | 评价 |
|---|---|---|
| **A(推荐)** 去掉垫片 | 删掉 `build/webview2-static/`、`build.rs` 与 `build.ps1` 里的删除逻辑,回到 `exe + WebView2Loader.dll` 双文件,由安装包分发 | 删掉一整块自制复杂度;风险是必须先确认 bundler 会把 DLL 打包进去 |
| B 保守 | 垫片原样保留,安装包只是换外壳 | 零回归风险,但垫片继续要维护 |

**选 A 的强制前置验证**(做完 A 立刻验,不验不许继续):

1. `cargo tauri build` 出安装包 → 安装 → 看安装目录里**有没有 `WebView2Loader.dll`**;
2. 没有就在 `bundle.resources` 里显式声明它;
3. 把安装出的 exe 单独拷进空目录启动,确认报的不是"找不到 DLL"。

> Tauri 官方只支持 MSVC target,GNU 属于非官方路径 —— bundler 是否自动收集
> exe 旁边的 DLL **没有文档保证**,必须实测。这条是本次最大的不确定性。

---

## 5. 更新机制(决策 D3)

**推荐:官方 `tauri-plugin-updater`,不是自研。**

核心理由只有一个:**签名校验不可省略**。不校验签名 = 一次网络劫持就能把任意 exe
塞进所有用户的机器(完整 RCE)。官方插件把签名、下载、校验、Windows 静默安装、
退出重启全部做对了;自研要自己重做一遍,其中"静默安装 + 自身退出"的竞态最坑。

| | 官方 updater | 自研(调 GitHub API + 跑 /S) |
|---|---|---|
| 防投毒 | 内置 minisign 校验 | 无(除非自己实现) |
| Windows 静默安装 | 已封装 | 自己处理参数/权限/退出时序 |
| 成本 | 依赖 + 一对密钥 + 一个 JSON | 代码量不小,坑在细节 |
| 可控性 | 一般 | 高 |

**代价(必须接受)**:需要一对签名密钥。**私钥一旦丢失,已安装的用户永远无法再
收到更新**,只能让他们手动重装。私钥 + 密码必须进密码管理器。

### 配置

```jsonc
"plugins": {
  "updater": {
    "pubkey": "<PUBLICKEY.pem 内容,不是文件路径>",
    "endpoints": [
      "https://github.com/wacilimonster-source/Grandettoken/releases/latest/download/latest.json"
    ],
    "windows": { "installMode": "passive" }
  }
}
```

`endpoints` 用 GitHub Releases 的 `latest/download/latest.json` —— **这个 URL 永久
指向最新 release**,所以不需要自建服务器,正好满足"下载文件放 GitHub"。

`installMode: passive` = 一个带进度条的小窗口、无需交互。不要用 `quiet`
(它无法自行提权),也不要 `basicUi`(要用户点下一步)。

### capabilities

`app/src-tauri/capabilities/default.json` 的 permissions 数组加 `"updater:default"`
(含 check / download / install / download-and-install)。

---

## 6. 前端怎么用(不走 npm)

前端是无打包器的静态页(`withGlobalTauri`),拿不到 `@tauri-apps/plugin-updater`
的 JS 包。因此**更新逻辑放在 Rust**,前端只发命令 + 收事件 —— 和现有的
`get_channels` / `channels-updated` 完全是同一套路子。

### 新增命令

| 命令 | 返回 | 说明 |
|---|---|---|
| `check_update(force: bool)` | `UpdateState` | 带 24h 节流;`force=true` 用于手动点击 |
| `install_update()` | `()` | 后台走下载→校验→安装;Windows 上装完会自动退出进程 |
| `skip_update_version(v)` | `()` | 记进配置,此后不再提示该版本 |

### 新增事件

`update-progress`:`{ phase: "started"|"downloading"|"finished", received, total }`
→ 前端复用现有 `.bar` 进度条。

### Config 新增字段(`app/core/src/config.rs`)

```rust
/// 是否自动检查更新(默认 true)
pub auto_check_update: bool,
/// 上次检查的 unix 秒;节流用
pub last_check_at: Option<i64>,
/// 用户选择"跳过此版本"的版本号
pub skipped_version: Option<String>,
```

三个都用 `#[serde(default)]` —— 老配置 JSON 读进来不出错。

---

## 7. 更新状态机

```
idle ──(启动后 30s / 手动点击 / 距上次 > 24h)──▶ checking
checking ──┬─ 无新版 / 已跳过 / 网络失败 ──▶ idle(静默,不打扰)
           └─ 有新版本 ──▶ available
available ──┬─ 稍后 ──▶ idle
            ├─ 跳过此版本 ──▶ idle(记 skippedVersion)
            └─ 立即更新 ──▶ downloading
downloading ──(进度: 0→100%)──▶ verifying
verifying ──┬─ 签名不通过 ──▶ failed(硬失败,明确告警,禁止静默吞掉)
            └─ 通过 ──▶ installing(Windows 上进程自动退出)
downloading/verifying 任一出错 ──▶ failed(保留重试 + 手动下载链接)
```

**节流与失败策略**:网络失败、GitHub API 403 限流 → 只更新 `lastCheckAt`,
不弹窗、不重试风暴。只有"发现新版本"和"手动点击"才算成功反馈。

---

## 8. UI 设计

### 8.1 管理页新增「关于」分区(在「启动」之后)

```
关于
  版本 0.2.0 · 构建 2026-09-17
  上次检查:今天 11:20            [检查更新]
  ─ 开关 ─
  自动检查更新                      ●━━━
```
版本号走 Rust `env!("CARGO_PKG_VERSION")`,不写死在前端。

### 8.2 底栏提示(不打断)

发现新版本时,底栏左侧状态点变蓝,出现「**有新版本 0.2.0**」链接 →
点进去打开管理页并滚到更新卡片。保持"常驻挂件不弹窗"的产品调性。

### 8.3 更新卡片

```
┌─ 发现新版本 ──────────────────────┐
│ 0.2.0 · 2026-09-17               │
│                                  │
│ · 安装包交付,不再需要 WebView2Loader │
│ · 新增检查更新                     │
│ · 修复胶囊圆角外的直角底色           │
│                                  │
│ [立即更新]  [跳过此版本]  [稍后]     │
└──────────────────────────────────┘
```
更新说明取 release notes(截断到前 3~5 条;超长不撑开卡片)。

### 8.4 下载中 / 失败

- 下载中:复用 `.bar` 进度条 + 百分比 + `[取消]`
- 安装中:`即将重启完成安装`(Windows 上 install 会自动退出,别再弹确认)
- 失败:一行原因(`网络不可用` / `校验未通过` / `下载中断`)+ `[重试]` + `手动下载`
  (打开 GitHub release 页)

---

## 9. 发布流程(运维)

```bash
# 一次性:生成签名密钥(私钥进密码管理器,公钥写进 tauri.conf.json)
cargo tauri signer generate -w ~/.tauri/tokenscope.key

# 每次发布
1. 同时改 app/src-tauri/Cargo.toml 与 tauri.conf.json 的 version(必须一致)
2. 提交打 tag: git tag v0.2.0 && git push --tags
3. 带私钥构建:
   export TAURI_SIGNING_PRIVATE_KEY="~/.tauri/tokenscope.key"
   export TAURI_SIGNING_PRIVATE_KEY_PASSWORD="..."
   .\build.ps1 bundle
4. 上传 3 个资源到该 release:
     TokenScope_0.2.0_x64-setup.exe          # 人手动下载
     TokenScope_0.2.0_x64-setup.exe.sig      # 自动更新校验用
     latest.json                             # 更新清单
5. 发布
```

`latest.json` 里 `platforms["windows-x86_64"].url` 指向同一个 release 的
setup.exe 下载链接,`signature` 填 `.sig` 文件内容(不是路径)。

**顺手要做的**:卸载时清掉 HKCU Run 键(NSIS `installerHooks`),
否则卸载后残留一个指向已删除 exe 的自启项。

**可选进阶**:把发布构建搬到 GitHub Actions(MSVC 工具链)。好处是彻底摆脱
GNU 这条非官方路径、垫片可以连根拔掉(MSVC 原生静态链接 `WebView2LoaderStatic`)、
构建可复现。代价是要写 workflow + 把私钥放 secrets + 做一次 MSVC 产物回归。
建议先把"本地 GNU + 安装包 + 更新"跑通,再考虑这一步。

---

## 10. 迁移与卸载

- **老用户从绿色版迁移**:手动装一次安装包即可。用户数据在
  `%APPDATA%\com.wacil.tokenscope\tokenscope.db`(密钥在 Windows 凭据管理器),
  **都不在安装目录里**,所以升级/重装/迁移都不丢 —— 这是 per-user 安装位置的
  关键好处。
- **自启**:不用改。setup 里"每次启动按 `current_exe()` 重写 Run 键"的逻辑,
  装完第一次启动就会自动纠正到新路径。
- **卸载**:默认保留数据库(里面有历史快照),提示里说明;
  询问是否同时清除已保存的密钥。

---

## 11. 风险清单(按严重度)

| # | 风险 | 影响 | 应对 |
|---|---|---|---|
| R1 | GNU 非官方路径下 NSIS bundler 是否收集 `WebView2Loader.dll` 无文档保证 | 装完跑不起来 | 第 4 节的强制前置验证;没带上就加 `bundle.resources` |
| R2 | `cargo-tauri` 未安装(`~/.cargo/bin` 里没有) | `build.ps1 bundle` 直接失败 | 前置:`cargo install tauri-cli`(要联网,编译几分钟) |
| R3 | 私钥丢失 | 已装用户永久无法更新 | 私钥 + 密码进密码管理器,写进本文件醒目位置 |
| R4 | 版本写错 / 两处版本不一致 | 更新检测到自己、或永远检测不到 | 发版脚本里加一条断言:两处版本号必须相等 |
| R5 | 无网络 / GitHub 限流 | 检查失败 | 静默跳过,不打扰(第 7 节) |
| R6 | 装到别人机器上时 WebView2 缺失 | 打不开 | `embedBootstrapper` + `minimumWebview2Version` |
| R7 | 更新后行为差异(GNU 构建 vs 将来 MSVC 构建) | 难排查 | 现阶段不混用:要么全本地 GNU,要么全 CI MSVC |

---

## 12. 实施步骤

| 步 | 内容 | 是否阻塞 |
|---|---|---|
| 1 | 确认 D1(下线绿色版)、D2(去垫片)、D3(官方 updater) | ✅ 需你确认 |
| 2 | 装 `cargo-tauri`;跑通 `build.ps1 bundle` 出安装包 | 阻塞后续 |
| 3 | 前置验证 R1:安装目录里是否有 `WebView2Loader.dll` | 决定 D2 能否执行 |
| 4 | 生成签名密钥;改 `tauri.conf.json`(bundle + plugins.updater);加 capabilities | |
| 5 | Rust:`check_update` / `install_update` / `skip_update_version` + `update-progress` 事件 | |
| 6 | Config 三个新字段(带 `serde(default)`) | |
| 7 | 前端:管理页「关于」分区 + 更新卡片 + 进度条 + 失败态 | |
| 8 | 装一份旧版本 → 发一个更高的 release → 走通"检测→下载→安装→重启"全链路 | 验收 |
| 9 | 卸载器清 HKCU Run 键;写一份 `RELEASE.md` 把第 9 节流程固化 | |

---

## 13. 待确认清单

1. **D1**:绿色单 exe 是否下线?(推荐下线)
2. **D2**:是否去掉 `build/webview2-static/` 那套垫片?(推荐去掉,但先过 R1 验证)
3. **D9 进阶**:发布构建要不要搬 GitHub Actions(MSVC)?(建议先不做)
4. 版本号起点:从 `0.1.0` 直接到 `0.2.0`,还是先 `0.1.1` 走一轮?
   (建议 `0.2.0` —— 交付形态变更 + 新功能,够 minor)
