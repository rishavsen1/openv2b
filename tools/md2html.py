#!/usr/bin/env python3
"""Convert a Markdown report to a standalone styled HTML file.

Usage: python3 tools/md2html.py <input.md> [output.html]

Uses the `markdown` package when available (pip/uv install markdown); otherwise
falls back to a small built-in converter covering the subset used by the
reports (headings, lists, tables, fenced code, bold/italic/inline code, links).
The Markdown file stays the single source of truth; never edit the HTML.
"""

from __future__ import annotations

import html
import pathlib
import re
import sys

CSS = """
:root { color-scheme: light dark; }
body { font-family: -apple-system, 'Segoe UI', Roboto, sans-serif; line-height: 1.55;
       max-width: 60rem; margin: 2rem auto; padding: 0 1rem; }
@media (prefers-color-scheme: dark) { body { background: #111; color: #ddd; }
  a { color: #7ab7ff; } th { background: #222; } tr:nth-child(even) { background: #1a1a1a; }
  code, pre { background: #222; } }
h1, h2, h3 { line-height: 1.25; }
h1 { border-bottom: 2px solid #888; padding-bottom: .3rem; }
h2 { border-bottom: 1px solid #8884; padding-bottom: .2rem; margin-top: 2rem; }
table { border-collapse: collapse; margin: 1rem 0; display: block; overflow-x: auto; }
th, td { border: 1px solid #8886; padding: .35rem .6rem; text-align: left; }
th { background: #eee; }
tr:nth-child(even) { background: #8881; }
code { background: #8882; padding: .1rem .3rem; border-radius: 3px; font-size: .92em; }
pre { background: #8882; padding: .8rem; border-radius: 6px; overflow-x: auto; }
pre code { background: none; padding: 0; }
blockquote { border-left: 4px solid #888; margin-left: 0; padding-left: 1rem; color: #888; }
.ok { color: #2a2; font-weight: 600; } .fail { color: #c33; font-weight: 600; }
"""


def convert_with_library(text: str) -> str | None:
    try:
        import markdown  # type: ignore
    except ImportError:
        return None
    return markdown.markdown(text, extensions=["tables", "fenced_code", "toc"])


def convert_builtin(text: str) -> str:
    """Minimal Markdown subset converter (used when `markdown` is absent)."""

    def inline(s: str) -> str:
        s = html.escape(s, quote=False)
        s = re.sub(r"`([^`]+)`", r"<code>\1</code>", s)
        s = re.sub(r"\*\*([^*]+)\*\*", r"<strong>\1</strong>", s)
        s = re.sub(r"(?<!\w)\*([^*]+)\*(?!\w)", r"<em>\1</em>", s)
        s = re.sub(r"\[([^\]]+)\]\(([^)]+)\)", r'<a href="\2">\1</a>', s)
        return s

    out: list[str] = []
    lines = text.splitlines()
    i = 0
    in_list = False
    while i < len(lines):
        line = lines[i]
        if line.startswith("```"):
            if in_list:
                out.append("</ul>")
                in_list = False
            block: list[str] = []
            i += 1
            while i < len(lines) and not lines[i].startswith("```"):
                block.append(lines[i])
                i += 1
            out.append("<pre><code>" + html.escape("\n".join(block)) + "</code></pre>")
            i += 1
            continue
        heading = re.match(r"^(#{1,6})\s+(.*)", line)
        if heading:
            if in_list:
                out.append("</ul>")
                in_list = False
            level = len(heading.group(1))
            out.append(f"<h{level}>{inline(heading.group(2))}</h{level}>")
            i += 1
            continue
        if "|" in line and i + 1 < len(lines) and re.match(r"^\s*\|?[\s:|-]+\|[\s:|-]*$", lines[i + 1]):
            if in_list:
                out.append("</ul>")
                in_list = False
            headers = [c.strip() for c in line.strip().strip("|").split("|")]
            out.append("<table><thead><tr>" + "".join(f"<th>{inline(h)}</th>" for h in headers) + "</tr></thead><tbody>")
            i += 2
            while i < len(lines) and "|" in lines[i] and lines[i].strip():
                cells = [c.strip() for c in lines[i].strip().strip("|").split("|")]
                out.append("<tr>" + "".join(f"<td>{inline(c)}</td>" for c in cells) + "</tr>")
                i += 1
            out.append("</tbody></table>")
            continue
        bullet = re.match(r"^\s*[-*]\s+(.*)", line)
        if bullet:
            if not in_list:
                out.append("<ul>")
                in_list = True
            out.append(f"<li>{inline(bullet.group(1))}</li>")
            i += 1
            continue
        if in_list:
            out.append("</ul>")
            in_list = False
        if not line.strip():
            i += 1
            continue
        para = [line]
        while i + 1 < len(lines) and lines[i + 1].strip() and not re.match(r"^(#|```|\s*[-*]\s|.*\|)", lines[i + 1]):
            i += 1
            para.append(lines[i])
        out.append(f"<p>{inline(' '.join(para))}</p>")
        i += 1
    if in_list:
        out.append("</ul>")
    return "\n".join(out)


def main() -> int:
    if len(sys.argv) < 2:
        print(__doc__, file=sys.stderr)
        return 2
    src = pathlib.Path(sys.argv[1])
    dst = pathlib.Path(sys.argv[2]) if len(sys.argv) > 2 else src.with_suffix(".html")
    text = src.read_text(encoding="utf-8")
    body = convert_with_library(text) or convert_builtin(text)
    title_match = re.search(r"^#\s+(.+)$", text, re.MULTILINE)
    title = html.escape(title_match.group(1)) if title_match else src.stem
    dst.write_text(
        "<!doctype html><html><head><meta charset='utf-8'>"
        f"<meta name='viewport' content='width=device-width, initial-scale=1'><title>{title}</title>"
        f"<style>{CSS}</style></head><body>{body}</body></html>",
        encoding="utf-8",
    )
    print(dst)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
