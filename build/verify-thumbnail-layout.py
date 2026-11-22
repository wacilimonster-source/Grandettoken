"""开发期排版验证。需 pip install playwright 与系统 Edge。

先设置 THUMBNAIL_LAYOUT_FIXTURES 为本地输出路径并运行 node build/jsdom-smoke.js，
再运行 python build/verify-thumbnail-layout.py <该夹具路径> [--screenshot <本地PNG路径>]。
夹具来自真实行渲染，包含合成渠道。检查失败以非零退出，坏样本自检默认执行。
"""
import argparse
import json
import re
import sys
import tempfile
from pathlib import Path
from playwright.sync_api import sync_playwright

sys.path.insert(0, str(Path(__file__).resolve().parent))
from lib.app_sources import app_source as read_app_source


ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "app" / "src"


def measure(page):
    return page.evaluate("""() => {
      const list = document.getElementById('list');
      const rows = [...list.querySelectorAll(':scope > .row')];
      const bounds = list.getBoundingClientRect();
      const foot = document.querySelector('.foot');
      return {
        count: rows.length,
        lastVisible: rows.at(-1).getBoundingClientRect().bottom <= bounds.bottom - parseFloat(getComputedStyle(list).paddingBottom) + 1,
        columns: getComputedStyle(list).gridTemplateColumns.split(' ').length,
        height: rows[0].getBoundingClientRect().height,
        overflow: document.documentElement.scrollWidth > innerWidth || foot.scrollWidth > foot.clientWidth + 1,
        clipped: rows.filter(row => { const value = row.querySelector('.val .v'); return value.scrollWidth > value.clientWidth + 1; }).length,
        misaligned: rows.filter(row => {
          const value = row.querySelector('.val .v');
          const box = row.querySelector('.val').getBoundingClientRect();
          const label = row.querySelector('.thumb-window').getBoundingClientRect();
          return getComputedStyle(value).textAlign !== 'right' || Math.abs(value.getBoundingClientRect().right - box.right) > 1 || label.right > value.getBoundingClientRect().left + 1;
        }).length,
        statusMisaligned: rows.filter(row => {
          const status = row.querySelector('.thumb-status');
          const end = row.querySelector('.thumb-status-end');
          return Math.abs(status.getBoundingClientRect().right - end.getBoundingClientRect().right) > 1 || Math.abs(status.getBoundingClientRect().top - end.getBoundingClientRect().top) > 1 || end.scrollWidth > end.clientWidth + 1;
        }).length,
        weeklyClipped: [...list.querySelectorAll('.thumb-window .thumb-week-reset')].filter(element => element.scrollWidth > element.clientWidth + 1).length
      };
    }""")


def validate(result, count):
    assert result["count"] == count, result
    assert result["lastVisible"] and result["columns"] == 2, result
    assert not result["overflow"] and not result["clipped"], result
    assert not result["misaligned"] and not result["statusMisaligned"], result
    assert not result["weeklyClipped"], result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("fixture", type=Path)
    parser.add_argument("--screenshot", type=Path)
    args = parser.parse_args()
    data = json.loads(args.fixture.read_text(encoding="utf-8"))
    app_source = read_app_source(SOURCE)
    date_helpers = "\n".join(re.findall(r"^function (?:fmtDay|fmtClock)\(ts\) \{.*?^\}", app_source, re.M | re.S))
    assert date_helpers, "日期格式依赖必须从 app.js 读取"
    escape_helper = re.search(r"^const esc =.*?^  \);", app_source, re.M | re.S)
    assert escape_helper, "日期节点的转义依赖必须从 app.js 读取"
    date_helpers += "\n" + escape_helper.group(0)
    styles = "".join("<style>" + path.read_text(encoding="utf-8") + "</style>" for path in SOURCE.glob("*.css"))
    body = re.sub(r"<script\b[^>]*>[\s\S]*?</script>", "", data["body"])
    document = ('<!doctype html><html><head><meta charset="utf-8"><base href="' + SOURCE.as_uri() + '/">'
                + styles + '</head><body class="form-panel layout-thumbnail">' + body + '</body></html>')
    with tempfile.TemporaryDirectory(prefix="gt-thumbnail-") as temporary:
        preview = Path(temporary) / "preview.html"
        preview.write_text(document, encoding="utf-8")
        with sync_playwright() as playwright:
            browser = playwright.chromium.launch(channel="msedge", headless=True)
            page = browser.new_page(viewport={"width": 500, "height": 840})
            errors = []
            page.on("pageerror", lambda error: errors.append(str(error)))
            page.goto(preview.as_uri())
            page.evaluate("""ids => {
              window.CHANNELS = ids.map(id => ({id, hasKey:true}));
              window.CFG = {showUsed:false};
              window.fsU = () => window.fixtureU || 1;
              window.currentForm = () => 'panel';
              window.setTextIfChanged = (element, text) => { if (element) element.textContent = text; };
              window.plainText = html => { const element = document.createElement('div'); element.innerHTML = html; return element.textContent; };
            }""", data["channels"])
            page.add_script_tag(content=(SOURCE / "panel-layout.js").read_text(encoding="utf-8"))
            page.add_script_tag(content=date_helpers)
            for unit in [1, 1.15, 1.3]:
                for light in [False, True]:
                    page.set_viewport_size({"width": round(480 * unit) + 20, "height": round(820 * unit) + 20})
                    page.evaluate("""settings => {
                      window.fixtureU = settings.unit;
                      document.documentElement.style.setProperty('--u', settings.unit);
                      document.body.classList.toggle('light', settings.light);
                      document.getElementById('toast').className = '';
                      PanelLayoutUI.schedule();
                    }""", {"unit": unit, "light": light})
                    page.wait_for_timeout(80)
                    result = measure(page)
                    validate(result, len(data["channels"]))
                    print({"unit": unit, "light": light, **result})
            page.set_viewport_size({"width": 500, "height": 840})
            page.evaluate("window.fixtureU=1;document.documentElement.style.setProperty('--u',1);document.body.classList.remove('light');PanelLayoutUI.schedule()")
            page.wait_for_timeout(80)
            for mutation in ["document.getElementById('list').style.gridTemplateColumns='1fr'",
                             "document.querySelector('#list>.row:last-child').style.height='800px'",
                             "document.querySelector('#list .val .v').style.textAlign='left'"]:
                page.evaluate(mutation)
                try:
                    validate(measure(page), len(data["channels"]))
                except AssertionError:
                    print("坏样本被拦截", mutation)
                else:
                    raise AssertionError("坏样本未报红 " + mutation)
                page.evaluate("document.getElementById('list').style.gridTemplateColumns='';document.querySelector('#list>.row:last-child').style.height='';document.querySelector('#list .val .v').style.textAlign='';PanelLayoutUI.schedule()")
                page.wait_for_timeout(80)
            if args.screenshot:
                page.screenshot(path=str(args.screenshot.resolve()))
            page.evaluate("""() => {
              document.querySelectorAll('.thumb-status-end [data-points-expiry]').forEach(element => {
                const at = new Date(2027, 0, 2, 10, 26).getTime() / 1000;
                element.outerHTML = PanelLayoutUI.expiryDateHtml(at);
              });
            }""")
            validate(measure(page), len(data["channels"]))
            page.evaluate("""() => {
              const list = document.getElementById('list');
              while (list.querySelectorAll(':scope > .row').length < 16) {
                const row = list.querySelector('.row').cloneNode(true);
                row.dataset.id = 'fixture-extra-' + list.children.length;
                row.classList.add('fixture-extra');
                list.append(row);
              }
              PanelLayoutUI.schedule();
            }""")
            page.wait_for_timeout(80)
            validate(measure(page), max(16, len(data["channels"])))
            page.evaluate("document.querySelectorAll('.fixture-extra').forEach(row=>row.remove());PanelLayoutUI.schedule()")
            page.evaluate("document.querySelectorAll('#list .val .v').forEach(el=>el.textContent=el.closest('.row').dataset.kind==='percent'?'100.0%':'USD 1,234,567.89');PanelLayoutUI.schedule()")
            page.wait_for_timeout(80)
            validate(measure(page), len(data["channels"]))
            for height in [760, 680]:
                page.set_viewport_size({"width": 500, "height": height})
                page.wait_for_timeout(80)
                result = measure(page)
                assert result["columns"] == 2 and not result["overflow"] and result["height"] >= 68, result
                if height == 760:
                    validate(result, len(data["channels"]))
                print({"clampedHeight": height, **result})
            assert not errors, errors
            with browser.new_context(timezone_id="America/New_York") as context:
                date_page = context.new_page()
                date_page.on("pageerror", lambda error: errors.append(str(error)))
                date_page.goto(preview.as_uri())
                date_page.evaluate("document.body.className='form-panel'")
                date_page.add_script_tag(content=date_helpers)
                date_page.add_script_tag(content=(SOURCE / "panel-layout.js").read_text(encoding="utf-8"))
                cases = date_page.evaluate("""() => {
                  const stamp = (year, month, day) => new Date(year, month - 1, day, 12).getTime();
                  return [
                    PanelLayoutUI.expiryDate(stamp(2026, 3, 9) / 1000, stamp(2026, 3, 7)),
                    PanelLayoutUI.expiryDate(stamp(2026, 11, 2) / 1000, stamp(2026, 10, 31)),
                    PanelLayoutUI.expiryDate(Date.UTC(2026, 10, 16, 1) / 1000, stamp(2026, 11, 15))
                  ];
                }""")
                assert cases == ["3-09 后天", "11-02 后天", "11-15 今天"], cases
                print("Local weekday and daylight-saving boundaries PASS")
                assert not errors, errors
            browser.close()


if __name__ == "__main__":
    main()
