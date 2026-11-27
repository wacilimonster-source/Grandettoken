const assert = require("node:assert/strict");
const { execFileSync } = require("node:child_process");
const path = require("node:path");

function blocked(files) {
  return files.filter(file =>
    /^build\/(?:qa(?:\/|$)|bundle-log-[^/]*\.txt$)/i.test(file) ||
    /\.(?:apk|exe|msi|zip|har)$/i.test(file)
  );
}

if (process.argv.includes("--self-test")) {
  const evidence = ["BUILD/qa/fixture.png", "build/bundle-log-demo.txt", "nested/demo.APK", "capture.har"];
  assert.deepEqual(blocked(["docs/assets/panel.png", "app/src/brand/mark.png", ...evidence]), evidence);
  console.log("产物门禁自检通过");
} else {
  const tracked = execFileSync("git", ["ls-files", "-z"], {
    cwd: path.resolve(__dirname, ".."),
    encoding: "utf8",
  }).split("\0").filter(Boolean);
  const violations = blocked(tracked);
  if (violations.length) {
    console.error(`Git 正在跟踪 QA 产物、安装包、抓包或打包日志（${violations.length} 项）`);
    process.exit(1);
  }
  console.log("Git 索引无受禁产物");
}
