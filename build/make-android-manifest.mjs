#!/usr/bin/env node
// 生成 Android 更新清单的 payload（未签名）。签名由 sign-android-manifest.py 负责。
//
// 字段与 android/app/src/main/java/com/wacil/grandettoken/sync/update/
// AndroidUpdateRepository.kt 的校验口径保持一致；改字段时两边一起改。
import { createHash } from "node:crypto";
import { readFileSync, statSync, writeFileSync } from "node:fs";
import { basename, resolve } from "node:path";

function value(name) {
  const i = process.argv.indexOf(`--${name}`);
  return i >= 0 ? process.argv[i + 1] : undefined;
}

function required(name) {
  const v = value(name);
  if (!v) throw new Error(`missing --${name}`);
  return v;
}

const versionName = required("version-name");
if (!/^\d+\.\d+\.\d+$/.test(versionName)) {
  throw new Error(`version-name must look like 1.2.3: ${versionName}`);
}
const versionCode = Number(required("version-code"));
if (!Number.isInteger(versionCode) || versionCode <= 0) {
  throw new Error(`version-code must be a positive integer: ${versionCode}`);
}
const tag = required("tag");
if (!/^android-v\d+\.\d+\.\d+$/.test(tag)) {
  throw new Error(`tag must look like android-v1.2.3: ${tag}`);
}
const apkPath = resolve(required("apk"));
const apk = readFileSync(apkPath);
const apkName = basename(apkPath);
const expectedName = `Grandettoken-android-v${versionName}.apk`;
if (apkName !== expectedName) {
  throw new Error(`apk name must be ${expectedName}, got ${apkName}`);
}
const repo = required("repo");
if (!/^[^/]+\/[^/]+$/.test(repo)) throw new Error(`repo must be owner/name: ${repo}`);
const notesFile = resolve(required("notes-file"));
const notes = readFileSync(notesFile, "utf8").trim();
if (notes.length < 10) throw new Error(`release notes too short (${notes.length} chars)`);

const sha256 = createHash("sha256").update(apk).digest("hex");
const sizeBytes = statSync(apkPath).size;
const downloadUrl = `https://github.com/${repo}/releases/download/${tag}/${apkName}`;
const payload = {
  schemaVersion: 1,
  channel: "stable",
  applicationId: "com.wacil.grandettoken.sync",
  versionName,
  versionCode,
  publishedAt: new Date().toISOString().replace(/\.\d{3}Z$/, "Z"),
  notes,
  releasePage: `https://github.com/${repo}/releases/tag/${tag}`,
  minSdk: 26,
  artifacts: [
    {
      abi: "universal",
      sizeBytes,
      sha256,
      downloadUrl,
    },
  ],
};

const out = resolve(required("out"));
writeFileSync(out, `${JSON.stringify(payload, null, 2)}\n`, "utf8");
console.log(`payload: ${out}`);
console.log(`version: ${versionName} (${versionCode})`);
console.log(`apk: ${apkName} ${sizeBytes} bytes sha256=${sha256}`);
