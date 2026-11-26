# 托盘数字 GDI 字体渲染审查（2026-11-26）

- **变更范围**：`app/core/src/tray_icon.rs`（+577/−22）、`app/core/Cargo.toml`（新增 `Win32_Graphics_Gdi`）
- **版本**：`v0.3.78`
- **结论**：通过，无 P0/P1 缺陷；保留 1 条 P3 观察项
- **原始证据**：`build/qa/review-2026-11-26/`（已被 `.gitignore`，不入库）

## 1. 变更内容

- 托盘徽标文字由自绘 5×7 点阵改为 Windows GDI 字体排版：字号从大到小寻找能放进徽标的最大字体，读取实际 ink bounds 后居中裁剪。
- 字体按 `Bahnschrift SemiCondensed` → `Bahnschrift Condensed` → `Segoe UI` → `Arial` 依次尝试，并用 `GetTextFaceW` 校验实际选中字体，避免字体替换后仍按原字体假设排版。
- 非 Windows 或 GDI 取字失败时回退到原点阵掩码；圆角底色、四种状态色、透明角、品牌图标还原逻辑保持不变。
- 新增回归：16/20/24/32/48/64 px 下多种文本的边界、居中、数字区分度、长文本回退与点阵掩码。

## 2. 自动化验证

| 项 | 结果 |
|---|---|
| `cargo test tray_icon::` | 4/4 通过（新增 `badge_stays_large_centered_and_unclipped`、`badge_long_text_fallback_...`） |
| core 全量单测 | **702 passed / 0 failed / 10 ignored** |
| `sync_fixtures` | 6 passed |
| `cargo check --all-targets --locked`（core/桌面） | 均 exit 0，**0 warning** |
| 静态门禁（IPC/静默/规模/CSS/编码/拆分/锁/临期/套餐） | 全部通过；IPC 111/111，规模仅剩既有预算 |
| 前端 jsdom + 6 项专项 | 317/317 与 6/6 通过 |
| `verify-refresh-stability --assert` | 20 场景通过，`childListCount=0` |
| 凭据扫描 | `CLEAN` |

## 3. 像素级与真机验证

- 渲染接触表 `build/qa/review-2026-11-26/badges/badge-sheet.png`：6 档尺寸 × 6 种文本（`74`/`12.5`/`1.2k`/`10w`/`100`/`-0.4`），全部居中、未越出内边距；20 px 以上清晰，16 px 下 4 字符文本较紧但仍完整可辨。
- 125% 真机（当前工作区 1920×1020）：托盘显示绿色 `94`、红色 `17`，字体居中无裁边；tooltip 与图标同步。
- 轮播：临时加入 Codex 后实测 `OpenCode Go 94.0%` → `Codex 17.0%` 切换正常。
- `trayShowNumber=false` 后立即还原品牌图标；重新打开恢复数字。
- GDI 对象数在启动、轮播、开关切换前后均为 **21 → 21**，用户对象 47 → 47，无 GDI 泄漏。
- 用户数据：测试前后 `tokenscope.db` / `-shm` / `-wal` SHA-256 全部 `MATCH`。

## 4. 观察项（不阻断）

| 编号 | 级别 | 说明 | 建议 |
|---|---|---|---|
| T-1 | P3 | `fit_with_face` 在 `TextOutW` 失败时返回全零掩码而不是 `None`，该路径下会显示无数字的纯色徽标；正常 Windows 环境未观察到，点阵回退仅在 GDI 返回 `None` 时触发 | 后续可改为返回 `None` 以落到点阵回退；不影响本次发版 |
| T-2 | P3 | 16 px 下 4 字符（如 `12.5`）偏紧，Windows 托盘实际按 DPI 取 20/24 px 时清晰 | 若未来支持 100% 缩放下的长文本，可再收紧字号搜索 |

## 5. 结论

变更集中、回退路径完整、字体替换有校验；自动化、像素级和 125% 真机验证均通过。可发布 `v0.3.78`。
