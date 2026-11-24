// Windows 原生 PowerShell，无 npm 依赖。验证官方回调白名单和缺失身份的拒绝路径。
const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const crypto = require("node:crypto");
const { spawnSync } = require("node:child_process");
if (process.platform !== "win32") throw new Error("此验证需要 Windows 系统 PowerShell");
const source = fs.readFileSync(path.join(__dirname, "../app/core/src/antigravity_capture.ps1"), "utf8");
const executable = path.join(process.env.SystemRoot, "System32/WindowsPowerShell/v1.0/powershell.exe");
const dir = fs.mkdtempSync(path.join(os.tmpdir(), "grandettoken-ag-fixture-"));
const output = path.join(dir, "account.json");
// 超时只防挂死，不参与白名单判定。CI runner 上第一次拉起 powershell.exe
// （模块扫描 + 整份回调脚本解析）实测能到 10 秒，用 10 秒会把冷启动误判成回调失败。
const TIMEOUT_MS = 30000;
function run(state) {
  if (fs.existsSync(output)) fs.unlinkSync(output);
  const script = `& { ${source} } -CaptureDirectory '${dir.replaceAll("'", "''")}'`;
  const encoded = Buffer.from(script, "utf16le").toString("base64");
  const result = spawnSync(executable, ["-NoProfile", "-NonInteractive", "-EncodedCommand", encoded], {
    input: JSON.stringify(state), encoding: "utf8", windowsHide: true, timeout: TIMEOUT_MS,
  });
  assert.equal(
    result.status,
    0,
    `回调执行应成功或明确拒绝无效样本（status=${result.status} signal=${result.signal} ` +
      `error=${result.error?.code ?? "-"} timeout=${TIMEOUT_MS}ms ` +
      `stderr=${(result.stderr ?? "").trim().slice(-400) || "-"}）`,
  );
  return fs.existsSync(output) ? JSON.parse(fs.readFileSync(output, "utf8")) : null;
}
try {
  const email = "Fixture@Example.Invalid";
  const state = { email, transcript_path: "fixture-private-path", extra: "fixture-sensitive-field", quota: {
    "gemini-weekly": { remaining_fraction: 0.68, reset_time: "2026-12-01T00:00:00Z", reset_in_seconds: 3600, extra: "fixture-sensitive-field" },
    "3p-5h": { remaining_fraction: 0, reset_time: "unparseable", reset_in_seconds: -5 },
    "disabled-weekly": { remaining_fraction: 1, disabled: true },
    "invalid-weekly": { remaining_fraction: 1.5 },
  } };
  const report = run(state);
  const verify = value => {
    assert.deepEqual(Object.keys(value).sort(), ["account", "quota"]);
    assert.equal(value.account, crypto.createHash("sha256").update(email.toLowerCase()).digest("hex").slice(0, 24));
    assert.deepEqual(Object.keys(value.quota).sort(), ["3p-5h", "gemini-weekly"]);
    assert.deepEqual(Object.keys(value.quota["gemini-weekly"]).sort(), ["remaining_fraction", "reset_in_seconds", "reset_time"]);
    assert.equal(value.quota["gemini-weekly"].remaining_fraction, 0.68);
    assert.equal(value.quota["gemini-weekly"].reset_in_seconds, 3600);
    assert.equal(value.quota["3p-5h"].remaining_fraction, 0);
    assert.equal(value.quota["3p-5h"].reset_time, null);
    assert.equal(value.quota["3p-5h"].reset_in_seconds, null, "越界 reset_in_seconds 必须丢弃而不是透传");
    assert(!JSON.stringify(value).includes("fixture-"));
    assert(!JSON.stringify(value).includes("@"));
  };
  verify(report);
  assert.equal(run({ ...state, email: " fixture@example.invalid " }).account, report.account);
  assert.notEqual(run({ ...state, email: "another@example.invalid" }).account, report.account);
  assert.equal(run({ quota: state.quota }), null);
  assert.equal(run({ email, quota: {} }), null);
  assert.throws(() => verify({ ...report, email }), "泄露邮箱坏样本必须判红");
  assert.throws(() => verify({ ...report, account: "b".repeat(24) }), "身份串号坏样本必须判红");
  assert.throws(() => verify({ ...report, quota: {} }), "漏额度坏样本必须判红");
  console.log("Antigravity 回调白名单、身份隔离、零额度、初始化缺字段与反向哨兵 PASS");
} finally {
  for (const name of ["account.json", "account.tmp"]) {
    const file = path.join(dir, name);
    if (fs.existsSync(file)) fs.unlinkSync(file);
  }
  fs.rmdirSync(dir);
}
