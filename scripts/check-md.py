#!/usr/bin/env python3
"""Markdown 四类禁字门禁(家族标准《中英文 Markdown 技术文档字符与标点硬禁令》)。

error 级四类:破折号/连接号、Unicode 箭头、emoji、智能引号与全角字母数字。
豁免:YAML frontmatter、围栏代码块、行内代码、markdown 链接、裸 URL。
零第三方依赖;违规 exit 1,干净 exit 0。CI 与本地同款。

用法:python3 scripts/check-md.py [路径...] (默认扫描仓库根的 README/AGENTS/CHANGELOG/docs)
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

DASHES = "—–―−ー"
DOUBLE_DASH = "——"
ARROWS = re.compile("[←-⇿➜⬅⬆⬇⬀-⬏]")
EMOJI = re.compile("[☀-➿\U0001f000-\U0001faff✅⚠]")
SMART_QUOTES = re.compile("[“”‘’]")
FW_ALNUM = re.compile("[Ａ-Ｚａ-ｚ０-９]")
SKIP_DIRS = {".git", ".venv", "node_modules", "__pycache__", ".pytest_cache", ".claude",
             ".agents", ".rumdl_cache", "target", "dist", "build"}

_INLINE = re.compile(
    r"(`[^`]+`)|(\[[^\]]*\]\([^)\s]*(?:\([^)]*\)[^)\s]*)*\))|(https?://[^\s)\]]+)"
)


def strip_inline_exemptions(line: str) -> str:
    return _INLINE.sub("", line)


def line_violations(raw_line: str, in_fence: bool) -> list[str]:
    if in_fence or raw_line.lstrip().startswith("```"):
        return []
    line = strip_inline_exemptions(raw_line)
    out = []
    if DOUBLE_DASH in line or any(c in line for c in DASHES):
        out.append("破折号/连接号")
    if ARROWS.search(line):
        out.append("Unicode 箭头")
    if EMOJI.search(line):
        out.append("emoji")
    if SMART_QUOTES.search(line):
        out.append("智能引号")
    if FW_ALNUM.search(line):
        out.append("全角字母数字")
    return out


def file_violations(text: str) -> dict[int, list[str]]:
    out: dict[int, list[str]] = {}
    in_fence = False
    lines = text.splitlines()
    start = 0
    if lines[:1] == ["---"]:
        close = next((i for i in range(1, len(lines)) if lines[i] == "---"), None)
        if close is not None:
            start = close + 1
    for i in range(start, len(lines)):
        if lines[i].lstrip().startswith("```"):
            in_fence = not in_fence
            continue
        bad = line_violations(lines[i], in_fence)
        if bad:
            out[i + 1] = bad
    return out


def default_targets(root: Path) -> list[Path]:
    files = [p for p in [root / "README.md", root / "AGENTS.md", root / "CHANGELOG.md"] if p.is_file()]
    docs = root / "docs"
    if docs.is_dir():
        files.extend(p for p in docs.rglob("*.md") if not any(part in SKIP_DIRS for part in p.parts))
    return files


def main(argv: list[str]) -> int:
    root = Path(__file__).resolve().parent.parent
    args = argv[1:]
    targets = [Path(a) for a in args] if args else default_targets(root)
    failures = 0
    for path in targets:
        text = path.read_text(encoding="utf-8")
        bad = file_violations(text)
        if bad:
            failures += 1
            for line, classes in bad.items():
                print(f"{path}:{line}: {'、'.join(classes)}", file=sys.stderr)
    if failures:
        print(f"md 禁字门禁:{failures} 个文件违规(四类禁字,详见上行)", file=sys.stderr)
        return 1
    print(f"md 禁字门禁:{len(targets)} 个文件干净")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
