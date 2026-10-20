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
