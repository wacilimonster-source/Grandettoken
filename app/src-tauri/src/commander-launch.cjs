// Embedded in the executable. Wait for Windows job ownership before spawning npm.
const { spawn } = require("node:child_process");
const path = require("node:path");
process.stdin.once("data", () => {
  process.stdin.destroy();
  const [major, minor] = process.versions.node.split(".").map(Number);
  if (major < 22 || (major === 22 && minor < 12)) {
    console.error("DC_NODE_TOO_OLD");
    process.exit(1);
  }
  const cli = path.join(path.dirname(process.execPath), "node_modules", "npm", "bin", "npx-cli.js");
  const child = spawn(process.execPath, [cli, "--yes", "@wonderwhy-er/desktop-commander@0.3.4", "remote"], {
    windowsHide: true, stdio: ["ignore", "inherit", "inherit"],
  });
  child.on("error", () => { console.error("DC_NPM_FAILED"); process.exitCode = 1; });
  child.on("exit", code => { process.exitCode = code ?? 1; });
});
