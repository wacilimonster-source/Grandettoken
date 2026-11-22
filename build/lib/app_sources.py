"""Helper for reading app.js / app-*.js sources in index.html load order."""
from pathlib import Path
import re

DEFAULT_SRC = Path(__file__).resolve().parents[2] / "app" / "src"
_SCRIPT_RE = re.compile(r'<script\b[^>]*\bsrc="([^"]+)"[^>]*>\s*</script>')
_APP_NAME_RE = re.compile(r"^app(?:-.+)?\.js$")


def app_file_names(src_dir: Path = DEFAULT_SRC) -> list[str]:
    html_path = Path(src_dir) / "index.html"
    html = html_path.read_text(encoding="utf-8")
    names = [m.group(1) for m in _SCRIPT_RE.finditer(html) if _APP_NAME_RE.match(m.group(1))]
    if not names:
        raise RuntimeError(f"未在 {html_path} 中找到 app 脚本标签")
    return names


def app_files(src_dir: Path = DEFAULT_SRC) -> list[Path]:
    base = Path(src_dir)
    return [base / name for name in app_file_names(base)]


def app_source(src_dir: Path = DEFAULT_SRC) -> str:
    return "".join(path.read_text(encoding="utf-8") for path in app_files(src_dir))


def strip_app_script_tags(html: str) -> str:
    return re.sub(r'<script\b[^>]*\bsrc="app(?:-.+)?\.js"[^>]*>\s*</script>\s*', "", html)
