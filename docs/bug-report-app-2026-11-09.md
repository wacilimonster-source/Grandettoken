# App 端 Bug 排查报告(2026-11-09)

排查范围:`android/app/src/main` 全部 31 个 Kotlin 文件(约 6000 行)+ AndroidManifest + 小部件配置 XML,
覆盖同步链路、加密存储、小部件、通知、更新、登录六大模块。按严重度排序,共 8 项。

**全部 8 项已确认修复,随 Android 0.1.12(versionCode 13)发布**(tag `android-v0.1.12`)。

| # | 严重度 | 问题 | 状态 |
|---|--------|------|------|
| 1 | 高 | 小部件不自动刷新,只能手动点 | 已修复(Android 0.1.12) |
| 2 | 中 | 手动刷新离线时假"刷新中",永不提示失败 | 已修复(Android 0.1.12) |
| 3 | 中 | 切回 App 不立即刷新,显示旧数据 | 已修复(Android 0.1.12) |
| 4 | 中 | 退出同步组不清 GitHub 登录,换组后静默沿用 | 已修复(Android 0.1.12) |
| 5 | 低 | 首次配置小部件后一直"等待首次同步" | 已修复(Android 0.1.12) |
| 6 | 低 | 更新自动检查失败后 24 小时不再重试 | 已修复(Android 0.1.12) |
| 7 | 低 | 通知 ID 碰撞可能互相覆盖 | 已修复(Android 0.1.12) |
| 8 | 低 | 每次请求新建 OkHttpClient,轮询下浪费资源 | 已修复(Android 0.1.12) |

---

## 1.(高)小部件不自动刷新,只能手动点

**现状**
- `widget_card_info.xml` / `widget_strip_info.xml` 里 `updatePeriodMillis="0"`,桌面从不周期唤醒小部件,
  自愈链路(`WidgetSyncBridge.onWidgetShown`)只在小部件添加/重启时才有机会执行。
- `WidgetRefreshWorker.enqueue` 用 `ExistingWorkPolicy.KEEP`,断网时挂起的旧请求会把后续所有刷新请求
  (包括自动的)静默丢弃。

**修复方案**(已改,未提交)
- `updatePeriodMillis` 0 → 1800000(30 分钟,系统最小值);
- `KEEP` → `REPLACE`。

**修复后效果**
桌面每 ~30 分钟兜底重绘 + 缓存超 15 分钟自动补拉;即使 App 进程被杀、后台任务被推迟,
小部件数据最迟 30 分钟自动更新,"较旧/数据正常"状态也会按当前时间重新计算,不再冻结。

## 2.(中)手动刷新离线时假"刷新中",永不提示失败

**现状**
点小部件刷新按钮 → `markRefreshing(45 秒)` + 入队带 `NetworkType.CONNECTED` 约束的一次性任务
(`WidgetProviders.kt:41-44`、`WidgetRefreshWorker.kt:33-37`)。设备离线时任务一直挂起,
"刷新中"标签 45 秒后过期,之后没有任何重绘和失败提示,用户以为刷新过了,数据其实没动。

**修复方案**
去掉 `WidgetRefreshWorker` 的网络约束,让它立即执行:离线时 `GistClient` 几秒内抛网络异常 →
`RefreshResult.Error(NETWORK)` → `WidgetSyncBridge.onRefreshResult` → 小部件显示"离线/失败"。
`SyncWorker`(周期任务)保留约束不动。

**修复后效果**
点刷新要么成功更新,要么立刻明确显示"离线",不再出现无声的假刷新状态。

## 3.(中)切回 App 不立即刷新,显示旧数据

**现状**
`MainActivity` 的 ON_START 只调 `viewModel.startPolling()`(`MainActivity.kt:190`),轮询首轮要等
60 秒(登录)/300 秒(匿名)才发起(`SyncViewModel.kt:189`)。用户从别的 App 切回来,看到的
永远是离开前的旧数据,可能误以为"App 也不会自动刷新"。

**修复方案**
ON_START 时在 `startPolling()` 之后补一次 `viewModel.refresh()`(非 force,走 ETag/限流门,
刚刷过就是一次 304,代价极低)。

**修复后效果**
回到 App 秒级看到最新数据;后台刚刷过时仅一次 304,不消耗限流额度。

## 4.(中)退出同步组不清 GitHub 登录,换组后静默沿用

**现状**
`GroupRepository.logout()` → `SyncStore.clearGroup()`(`SyncStore.kt:115-124`)清掉主密钥、缓存、
水位,但 **GitHub token、登录名、Client ID 都保留**。重新扫另一个同步码加入后,`hasToken()` 为
true,界面直接显示"已登录"并按 60 秒轮询——用户没有做过任何登录操作。退出弹窗文案
(`MainActivity.kt:1025`)也只说删除"主密钥、缓存和后台任务",没提登录会保留。

**修复方案**
`logout()` 时一并 `clearToken()` + `clearGithubLogin()`;弹窗文案同步说明"GitHub 登录也会退出"。

**修复后效果**
退出即彻底清理本机凭据;换同步组后从匿名模式干净起步,不残留旧账号状态。

## 5.(低)首次配置小部件后一直"等待首次同步"

**现状**
缓存为空时在配置页选完条目点保存,小部件显示"等待首次同步",但保存动作不触发任何补拉
(`WidgetConfigActivity.kt:78-86`),要等下一个轮询周期(最坏 30 分钟)才有数据。

**修复方案**
`onSave` 里保存后调用 `WidgetSyncBridge.onWidgetShown(this)`(已有 5 分钟节流,不会重复拉)。

**修复后效果**
配置完成几秒内小部件直接出数据。

## 6.(低)更新自动检查失败后 24 小时不再重试

**现状**
ON_START 的自动检查在发请求**之前**就写入 `lastAutoAttempt`(`MainActivity.kt:196`),那次如果
断网失败,要再等 24 小时才会自动检查,期间用户得不到任何提示。

**修复方案**
检查失败时回滚 `lastAutoAttempt`(或失败改写 6 小时间隔);成功才记录本次检查。

**修复后效果**
断网场景下恢复网络后半天内能自动补查更新,而不是隔天。

## 7.(低)通知 ID 碰撞可能互相覆盖

**现状**
通知 ID 取 `(key.hashCode() & 0x7FFFFFFF) % 100_000`(`NotificationHelper.kt:157`),不同 key
(如两个渠道的余额提醒)有约十万分之一的碰撞概率,撞上时后发通知覆盖先发,用户少收一条。

**修复方案**
去掉 `% 100_000`,直接用 `key.hashCode() and 0x7FFFFFFF`。

**修复后效果**
碰撞窗口降到几乎为零,余额提醒逐渠道独立送达。

## 8.(低)每次请求新建 OkHttpClient,轮询下浪费资源

**现状**
`GroupRepository.fetchWithToken` 每次刷新 new 一个 `GistClient`(内部新建 OkHttpClient),
用完 `close()` 销毁线程池(`GroupRepository.kt:287-294`)。前台登录态 60 秒一轮询 =
每分钟创建/销毁整套线程池和连接池,增加延迟和内存抖动。

**修复方案**
`GistClient` 内改用模块级共享 OkHttpClient 单例;`close()` 保留签名但变空操作(调用方不用改)。

**修复后效果**
复用连接池,轮询更省电、请求延迟更低;行为与返回值完全不变。

---

## 检查过、无问题的部分

- **加密与存储**:Keystore 包装主密钥/token/缓存、HKDF 派生、GCM AAD 绑定来源与代数、同步码 CRC——实现正确;
- **更新安全链**:Ed25519 清单签名 + APK 哈希 + 证书指纹 + 受信主机白名单 + 各级大小上限,完整;
- **合并逻辑**:水位防回放、同 seq 冲突检测、部分失败时 `retainMissing` 保底——正确;
- **限流**:非 force 刷新统一走 `nextAllowed` 门,匿名 60 次/小时配额占用约 6 次,余量充足;
- **隐私**:无敏感日志、`allowBackup=false`、诊断只展示错误码计数。
