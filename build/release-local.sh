#!/usr/bin/env bash
# 本地打包。用法: bash build/release-local.sh
#
# 为什么要用 bash:RELEASE.md 记过一个坑 ——
#   在 PowerShell 里写 `$env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = ""` 等于**删掉**这个变量
#   (不是设成空串),签名步骤会去等一个永远不来的密码输入,CPU 全程 0 地挂住。
#   必须从 bash export 空串。
set -u

export TAURI_SIGNING_PRIVATE_KEY='C:\Users\wacil\.tauri\tokenscope.key'
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD=''
export PATH="/c/Users/wacil/.cargo/bin:/c/Users/wacil/mingw64/bin:$PATH"
export RUSTUP_TOOLCHAIN=stable-x86_64-pc-windows-gnu
export CARGO_HOME="/c/Users/wacil/.cargo"

cd /g/game/nw/Grandettoken || exit 1

echo "=== env 检查 ==="
if [ -n "${TAURI_SIGNING_PRIVATE_KEY:-}" ]; then echo "  私钥路径: 已设"; else echo "  私钥路径: 未设"; exit 1; fi
if [ -z "${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:-}" ]; then echo "  密码: 空串(正确)"; else echo "  密码: 非空(应为空)"; fi
echo "  toolchain: $(rustc -vV 2>/dev/null | grep host || echo '?')"
echo "  cargo-tauri: $(cargo tauri --version 2>/dev/null || echo '缺失')"
echo ""

echo "=== bundle ==="
powershell -NoProfile -ExecutionPolicy Bypass -File build/build.ps1 bundle
rc=$?
echo ""
echo "=== exit=$rc ==="
exit $rc
