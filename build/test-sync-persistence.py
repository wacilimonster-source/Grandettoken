"""Exercise the real sync-page JavaScript with a local, fake Tauri backend.

Run with Python Playwright installed. No network, real credentials or real sync codes are used.
"""
from pathlib import Path
from playwright.sync_api import sync_playwright
import json
import os
import re

ROOT = Path(__file__).resolve().parent.parent
SRC = ROOT / "app" / "src"
html = (SRC / "index.html").read_text(encoding="utf-8")
html = re.sub(r'<script\s+src="app\.js"\s*></script>', "", html)
script = (SRC / "app.js").read_text(encoding="utf-8")
start = script.index("function syncSummary()")
end = script.index("bindSyncUI();", start) + len("bindSyncUI();")

bootstrap = r'''
const $ = (id) => document.getElementById(id);
let CHANNELS = [], SYNC_STATUS = null, SYNC_STATUS_ERROR = null, SYNC_STATUS_REV = 0;
let SYNC_RECOVERY_BUSY = false, SYNC_CODE_REVEALED = false, SYNC_AUTH = null;
const TAB_TITLES = {sync: "同步到手机"};
const manageOpen = () => document.body.classList.contains("view-manage");
const renderManageIndex = () => {};
const relTime = () => "刚刚";
const esc = (x) => String(x).replace(/[&<>"']/g, (c) => ({"&":"&amp;","<":"&lt;",">":"&gt;",'"':"&quot;","'":"&#39;"}[c]));
const showToast = (message) => { window.testToast = message; };
window.confirm = () => window.testConfirm;
window.testConfirm = true;
window.testHandlers = {};
window.testCalls = [];
window.testFailure = false;
window.testDefer = false;
window.testHasSource = true;
window.testStatus = {
  relayId: "00112233445566778899aabbccddeeff", source: "s_00112233445566778899aabbccddeeff",
  hasGithub: true, githubLogin: "example", oauthClientConfigured: true,
  hasKey: true, enabled: true, codeText: "GT1.example-code-A", intervalSec: 60,
  channels: [], platformLabels: {}, lastError: null, keyError: null
};
const invoke = async (command, args) => {
  window.testCalls.push({ command, args });
  if (command === "sync_status") {
    if (window.testFailure) throw new Error("test store failure");
    if (window.testDefer) return new Promise(resolve => { window.testResolveStatus = resolve; });
    return JSON.parse(JSON.stringify(window.testStatus));
  }
  if (command === "sync_recovery_candidates") return {candidates:[{
    gistId:"11223344556677889900aabbccddeeff",keyEpoch:0,hasLocalSource:true
  }],unverified:0,truncated:false};
  if (command === "sync_restore_group") {
    window.testStatus = {...window.testStatus, relayId:args.gistId, codeText:"GT1.example-old", enabled:false,
      source:window.testHasSource ? window.testStatus.source : null};
    return {hasLocalSource:window.testHasSource};
  }
  if (command === "sync_create") {
    if (!args.allowExisting) throw new Error("SYNC_OLD_GROUP_FOUND");
    return {};
  }
  return {};
};
const listen = async (name, cb) => { window.testHandlers[name] = cb; };
document.body.classList.add("view-manage");
'''

with sync_playwright() as playwright:
    browser = playwright.chromium.launch(channel="chrome", headless=True)
    page = browser.new_page(viewport={"width": 400, "height": 580})
    errors = []
    page.on("pageerror", lambda error: errors.append(str(error)))
    page.set_content(html, wait_until="domcontentloaded")
    page.add_style_tag(path=str(SRC / "styles.css"))
    page.add_script_tag(content=bootstrap + "\n" + script[start:end] + "\nwindow.testSync = {showTab, loadSyncStatus};")
    page.wait_for_function("window.testHandlers['sync-status'] && !document.getElementById('syncCodeCard').hidden")

    page.evaluate("testSync.showTab('sync')")
    page.wait_for_function("document.getElementById('syncCodeText').textContent === 'GT1.example-code-A'")
    if os.environ.get("SYNC_UI_SCREENSHOT"):
        page.wait_for_timeout(350)  # Allow the settings page's entrance transition to finish.
        page.screenshot(path=os.environ["SYNC_UI_SCREENSHOT"])
    page.evaluate("testSync.showTab('index'); testSync.showTab('sync')")
    assert page.locator("#syncCodeText").inner_text() == "GT1.example-code-A"
    assert page.locator("#syncSetup").is_hidden()
    assert page.locator("#syncClientIdBox").is_hidden()

    # An older in-flight status must not overwrite a newer backend event.
    page.evaluate("() => { testDefer = true; window.pendingSync = testSync.loadSyncStatus(); }")
    page.wait_for_function("typeof testResolveStatus === 'function'")
    page.evaluate("testHandlers['sync-status']({payload:{...testStatus, codeText:'GT1.newer-event'}})")
    page.evaluate("testResolveStatus({...testStatus, codeText:'GT1.stale-response'}); testDefer = false")
    page.evaluate("pendingSync")
    assert page.locator("#syncCodeText").inner_text() == "GT1.newer-event"
    page.evaluate("testHandlers['sync-status']({payload:testStatus})")

    page.locator("#syncOptionsBtn").click()
    page.locator("#syncCompareOpen").click()
    page.locator("#syncCompareInput").fill("GT1.example-code-A")
    page.locator("#syncCompareBtn").click()
    assert "完全相同" in page.locator("#syncCompareResult").inner_text()
    assert page.locator("#syncCompareInput").input_value() == ""

    page.locator("#syncRecoverOpen").click()
    page.locator("[data-recover-id]").wait_for()
    page.locator("[data-recover-id]").click()
    page.wait_for_function("document.getElementById('syncCodeText').textContent === 'GT1.example-old'")
    restored = page.evaluate("testCalls.find(x => x.command === 'sync_restore_group').args")
    assert restored["expectedCurrent"] == "00112233445566778899aabbccddeeff"
    assert not page.locator("#syncEnabled").is_checked()

    # Without a trustworthy writer identity, show the recovered code but block publishing.
    page.evaluate("testStatus = {...testStatus, source:null, enabled:false}; testHandlers['sync-status']({payload:testStatus})")
    assert page.locator("#syncEnabled").is_disabled()
    assert "发布来源无法确认" in page.locator("#syncErr").inner_text()

    page.evaluate("testFailure = true")
    page.evaluate("testSync.loadSyncStatus()")
    page.wait_for_function("!document.getElementById('syncStatusError').hidden")
    assert page.locator("#syncSetup").is_hidden()
    assert page.locator("#syncCodeCard").is_hidden()
    page.evaluate("testFailure = false")
    page.locator("#syncStatusRetry").click()
    page.wait_for_function("!document.getElementById('syncCodeCard').hidden")

    # A retained credential blocks creation until the user explicitly agrees.
    page.evaluate("testStatus = {...testStatus, relayId:null, source:null, codeText:null}; testSync.loadSyncStatus()")
    page.wait_for_function("!document.getElementById('syncSetup').hidden")
    page.evaluate("testConfirm = false")
    page.locator("#syncCreateBtn").click()
    page.wait_for_function("!document.getElementById('syncRecovery').hidden")
    attempts = page.evaluate("testCalls.filter(x => x.command === 'sync_create')")
    assert len(attempts) == 1 and attempts[0]["args"]["allowExisting"] is False
    assert not errors, json.dumps(errors, ensure_ascii=False)
    print("sync UI persistence, recovery, errors and duplicate-creation checks passed")
    browser.close()
