# 发版构建加速调研与实测

> 2026-10-20 · 基于本机实测 + 两次 CI 运行的步骤耗时

## 一、现状:一次发版 21.9 分钟

两次实测(步骤耗时,来自 GitHub Actions API):

| 步骤 | 153a32a | 39432943098 | 说明 |
|---|---|---|---|
| **Install cargo-tauri** | **10.9 分** | **11.1 分** | `cargo install tauri-cli --locked` 从源码编译 |
| **Build NSIS + sign** | 7.6 分 | 7.4 分 | Rust release 冷编译 + NSIS 打包 + 签名 |
| Run core tests | 1.4 分 | 1.4 分 | 271 个单测 |
| Install MinGW-w64 | 0.7 分 | 0.7 分 | msys2/setup-msys2 |
| Fetch src-tauri deps | 0.4 分 | 0.4 分 | `cargo fetch --locked` |
| 其余 | <1 分 | <1 分 | checkout / resolve / latest.json / upload / verify |
| **总计** | **21.8 分** | **21.9 分** | |

**结论:cargo-tauri 占了整整一半时间,而且缓存是失效的。**

## 二、为什么 cargo-tauri 的缓存从没生效

`release.yml` 里配了 `actions/cache`,key 是 `hashFiles('app/src-tauri/Cargo.lock')`。
但运行日志里是:

```
Cache not found for input keys: cargo-tauri-Windows-897d2d4b3cb31a44588ffbcf92c18d00f3b39d2e48ea58cce0f0addb9f1f1dd6
...
Finished `release` profile [optimized] target(s) in 10m 59s
```

**两个原因叠加:**

1. **key 绑在 `Cargo.lock` 上** —— 只要改了任何依赖(这次加了 `windows-sys`),hash 就变,
   缓存必然 miss。而代码注释里自己写的是"key 里带上 tauri-cli 的版本区间,改了版本区间才会重新编译",
   **与实际实现不符**。
2. **失败时缓存不会保存** —— `actions/cache` 的保存发生在 job 收尾,而这次 job 在
   `Verify` 步骤失败,`Post Cache cargo-tauri` 被 skipped。于是形成恶性循环:
   失败 → 不保存 → 下次还是 miss → 又编 11 分钟。

## 三、各项加速方案的实测收益

### 方案 1:用 npm 预编译的 tauri CLI 替代 `cargo install`(省 ~10.5 分)

`@tauri-apps/cli-win32-x64-msvc` 是**预编译的 Node native addon**,
实测下载 8.0 MB、解包出一个 16 MB 的 `cli.win32-x64-msvc.node`,不需要编译。

**但有一个硬约束(实测)**:它是 `.node` 插件,只能通过 `npx @tauri-apps/cli` 调用,
**不是** `cargo tauri`。而 `build/build.ps1:172` 写死了 `& cargo tauri build`。

所以这条方案要么:
- 改 `build.ps1` 支持两种调用方式(本地用 `cargo tauri`、CI 用 `npx`),或
- 在 CI 里装 `npx @tauri-apps/cli` 后把 `cargo-tauri` 做成一个转发到它的 shim

**风险**:npm 包是 **msvc** 构建,而本项目用 **GNU 工具链**(`RUSTUP_TOOLCHAIN=stable-x86_64-pc-windows-gnu`)。
tauri-cli 本身只是"调用 cargo + 打包",理论上不关心 target 的 ABI,
但**这一点没有实测验证**——需要在 CI 上真跑一次才能确认。

### 方案 2:缓存 Rust 依赖的编译产物(省 ~5 分)

实测本机 `target/` 规模:**28 GB**(release 部分 12 GB)。**整目录缓存不可行** ——
GitHub 缓存上限 10 GB/仓库,而且上传/下载 12 GB 比重编还慢。

但 `swatinem/rust-cache` 默认**只缓存依赖、不缓存 workspace 自身 crate**,
所以缓存体积远小于 28 GB。实测本机增量编译(依赖全在,只重编本 crate):
**3 分 05 秒**,而 CI 的 `Build` 步骤是 7.4 分 —— 其中 Rust 冷编译约 6.5 分、NSIS 打包约 1 分。
缓存依赖后 `Build` 预计降到 **2 分左右**。

### 方案 3:把 core tests 与 bundle 拆成两个并行 job(省 ~1.4 分)

两者互不依赖(测试只读代码)。但拆 job 后 bundle job 要重新装 MinGW(0.7 分),
净省约 0.7 分,**收益小、改动大**,不建议。

### 方案 4:合并三个 `cargo` 调用为一次

现在有 `cargo test`(core)→ `cargo fetch`(src-tauri)→ `cargo build`(src-tauri),
三次调用各自可能触发 registry 索引更新。合并收益很小(实测都 <0.5 分),不做。

## 四、建议的落地顺序

| 顺序 | 动作 | 省 | 风险 | 是否需改 build.ps1 |
|---|---|---|---|---|
| ① | `cargo-tauri` 缓存 key 去掉 `Cargo.lock`(改绑固定版本串) | 让缓存有机会命中 | 无 | 否 |
| ② | 加 `swatinem/rust-cache`(只缓存依赖) | ~5 分 | 低 | 否 |
| ③ | 失败也要保存 cargo-tauri 缓存(`if: always()` 或在 install 后立即 save) | 保证 ① 生效 | 无 | 否 |
| ④ | 用 npm 预编译 CLI 替代 `cargo install` | ~10.5 分 | **中**(msvc 二进制 + 需改 build.ps1) | **是** |

①②③ 加起来预计 **21.9 分 → 约 6 分**,且不动构建方式,风险很低。
④ 能进一步降到 **1–2 分**,但要先验证 msvc 二进制能不能驱动 GNU target 的 bundle。

## 四之二、社区调研:别人怎么加速,以及为什么我们抄不了

2026-10-20 用 GitHub 代码搜索 + 直接读仓库统计了主流做法。

### 做法 A:`taiki-e/install-action`(200+ 项目在用)—— **对 tauri-cli 无效**

这是 Rust CI 里装工具的社区标准做法(免编译,直接拉预编译二进制)。但实测:

| 检查 | 结果 |
|---|---|
| `install-action` 的 tools 白名单里有没有 tauri-cli | **没有**(tools 目录只有 8 个条目) |
| 不在白名单会怎样 | 回退到 `cargo-binstall` |
| `tauri-apps/tauri` 的 release assets | **0 个** |
| `cargo-quickinstall` 最近 100 个 release 里 tauri-cli | **命中 0** |

**结论:tauri-cli 根本没有官方预编译二进制可拿。** 这条"标准做法"落到 tauri-cli 上
等于还是 `cargo install`,那 11 分钟省不掉。

### 做法 B:npm 的 `@tauri-apps/cli` —— 有 GNU 分支的痕迹,但包不存在

`@tauri-apps/cli` 的 `index.js`(NAPI-RS 生成)里明确写着:

```js
return require('./cli.win32-x64-gnu.node')   // ← 优先 GNU
return require('./cli.win32-x64-msvc.node')
```

看着像有 GNU 版。但 **`@tauri-apps/cli-win32-x64-gnu` 在 npm 上是 404**(实测)。
所以 Windows 上只能装到 **msvc** 版 `.node` 二进制。

本项目用 **GNU 工具链**,而 `build/build.ps1:172` 写死了 `& cargo tauri build`。
要走这条路得:① 改 build.ps1 支持 npx 调用;② 赌 msvc 版 CLI 能驱动 GNU target 的
bundle —— **这两点都没有实测依据**。发版链路不能赌,所以不做。

### 做法 C:`tauri-apps/tauri-action`(spacedrive 等在用)—— **不省编译**

spacedrive 的 release.yml 核心只有:

```yaml
- name: Build with tauri-action
  uses: tauri-apps/tauri-action@v0
  with:
    profile: release
```

`tauri-action` 只是封装了 `cargo tauri build` + 上传 release,**底层照样全量编译**。
它不解决"装 CLI 要 11 分钟"这个我们特有的问题。

### 结论:我们已用的方案就是当前能拿到的最优

| 方案 | 别人用吗 | 我们能省 | 状态 |
|---|---|---|---|
| `Swatinem/rust-cache` 缓存依赖产物 | ✅ 广泛 | ~5 分 | ✅ 已上 |
| 缓存 cargo-tauri 二进制 + 稳定 key | ✅ 常见 | 11 分(次轮起) | ✅ 已上 |
| `taiki-e/install-action` | ✅ 200+ 项目 | **0**(tauri-cli 无预编译) | ❌ 实测无效 |
| `cargo-binstall` | ✅ | **0**(同上) | ❌ 实测无效 |
| npm 预编译 CLI | ⚠️ 少数 | 10.5 分 | ❌ 只有 msvc,风险高 |
| `tauri-apps/tauri-action` | ✅ | 0(不省编译) | ❌ 无收益 |

### 还想再快的两条路(都有代价,未做)

1. **自建 tauri-cli 预编译缓存**:手动编一次,把 `cargo-tauri.exe` 传到本项目自己的
   release(或 artifact),CI 直接下。能省 11 分,但**要自己维护一个二进制分发点**,
   且跨 runner 镜像版本可能 ABI 不兼容(需要按 `runner.os` + 镜像版本打 tag)。
2. **改用 msvc 工具链**:那样就能用 npm 预编译 CLI。但整个项目要改链接器配置,
   且 WebView2 加载器要从静态改为动态分发(PROGRESS 里记过:当初选 GNU 正是为了
   避免 MSVC 需要单独的 WebView2Loader.dll)。**改动面远超收益**。

## 五、顺带发现的两个发版正确性 bug(不是速度问题,但更要紧)

调研过程中重跑 v0.1.47,暴露出两个**会让发版"看起来成功、实际坏掉"**的问题。
它们与速度无关,但优先级更高 —— 所以一并修了。

### bug 1:`.sig` 上传后消失(自动更新链路会断)

`softprops/action-gh-release` 的日志逐字:

```
✅ Uploaded Grandettoken_0.1.47_x64-setup.exe
✅ Uploaded latest.json
error updating release asset metadata for Grandettoken_0.1.47_x64-setup.exe.sig:
  HttpError: Not Found - https://docs.github.com/rest/releases/assets#update-a-release-asset
  Matching asset is present after refresh; continuing...
```

它"认为"还在(所以不报错、job 继续往下走),但 **release 里最终就是没有那个 `.sig`** ——
三件套变两件。这是上传插件更新 asset metadata 的竞态。

**危害**:已装用户的自动更新**靠 `.sig` 验签**,缺了它更新直接失败。
而 release 已经发出去了,用户看得到、拉不到 —— 比发版失败更糟。

**修法**:上传后加一道幂等补传 —— 用 `gh` 逐个核对,缺了就 `gh release upload --clobber`,
重试 5 次仍缺就明确 `throw`。宁可发版失败,也不留一个半残的 release。

### bug 2:`workflow_dispatch` 补跑时 notes 为空

第一次修的是"提交信息被内插进 PowerShell 字符串"(引号导致 ParserError)。
但改成 env 传递后,**`workflow_dispatch` 事件没有 `head_commit`**,
`github.event.head_commit.message` 是空串 → notes 只剩标题。

实测:notes 长度 **24 字符、0 个中文**,verify 的「notes intact」判 FAIL。

**修法**:两级回落 —— `head_commit.message` 为空时,按 tag 反查
`git log -1 --pretty=%B <tag>`;再加一道断言(notes < 40 字符直接抛错),
防止"静默发一个空说明的版本"。

修后本地实测:notes **1336 字符、395 个中文**。

## 六、还没验证的(不写死结论)

- **npm 预编译 CLI 能否用于 GNU target 的 bundle** —— 未实测。这是 ④ 的前置条件。
- **`swatinem/rust-cache` 在 GNU 工具链 + MinGW 下的实际命中率与体积** —— 未实测。
  风险点:RUSTFLAGS 里带 `-C linker=...` 绝对路径,可能影响缓存 key 的稳定性。
- **缓存恢复本身要多久** —— 如果依赖产物有几 GB,恢复可能吃掉一半收益。
  需要在 CI 上跑一次看 `Cache restored` 的耗时。

## 七、附:本机实测数据(可复核)

```
# target 规模
target/ 总计          28,000 MB / 37,215 files
  debug               15,963 MB
  release             11,999 MB

# 增量 release 编译(依赖全在,只重编本 crate)
cargo build --release --locked   ->  3 分 05 秒(186.3 秒)

# npm 预编译 CLI
@tauri-apps/cli-win32-x64-msvc@2.12.3
  tarball 8,002,719 bytes
  解出 cli.win32-x64-msvc.node  16,085,504 bytes
  无独立 exe(是 Node native addon)
```
