#!/usr/bin/env node
"use strict";
const path = require("node:path");
const { spawnSync } = require("node:child_process");

const result = spawnSync("cargo", ["test", "--locked", "tray_icon::", "--", "--test-threads=1"], {
  cwd: path.resolve(__dirname, "../app/core"),
  stdio: "inherit",
});
if (result.error) console.error("无法运行 Rust 托盘测试", result.error.message);
process.exitCode = result.status ?? 1;
