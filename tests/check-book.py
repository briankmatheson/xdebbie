#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Check the mdBook in docs/ without mdbook installed.

  - every SUMMARY.md entry points at a file that exists;
  - every page under docs/ is in SUMMARY.md (nothing orphaned);
  - the book has the six parts, in order (docs/README.md);
  - every relative link in docs/**/*.md resolves, and so does its #anchor
    (headings slugged as mdBook does).

Usage: tests/check-book.py   (exit status 1 on any finding)
"""
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DOCS = os.path.join(ROOT, "docs")
SUMMARY = os.path.join(DOCS, "SUMMARY.md")
PARTS = ["getting-started/README.md", "network/README.md", "storage/README.md",
         "compute/README.md", "reference/README.md", "glossary.md"]
LINK = re.compile(r"(?<!!)\[[^\]]*\]\(([^)\s]+)(?:\s+\"[^\"]*\")?\)")
HEADING = re.compile(r"^#{1,6}\s+(.*?)\s*#*\s*$")


def slug(text):
    """mdBook's heading id: link text kept, lowercased, spaces to '-',
    anything but letters, digits, '_' and '-' dropped."""
    text = re.sub(r"\[([^\]]*)\]\([^)]*\)", r"\1", text).strip().lower()
    return "".join("-" if c.isspace() else c for c in text
                   if c.isalnum() or c in "_-" or c.isspace())


def strip_code(lines):
    """Lines outside fenced code blocks, with inline code removed."""
    fence = False
    for n, line in enumerate(lines, 1):
        if line.lstrip().startswith("```"):
            fence = not fence
            continue
        if not fence:
            yield n, re.sub(r"`[^`]*`", "", line)


def heading_text(path):
    """Headings again, but with inline code kept (it is part of the id)."""
    with open(path, encoding="utf-8") as fh:
        lines = fh.read().splitlines()
    fence, out = False, []
    for line in lines:
        if line.lstrip().startswith("```"):
            fence = not fence
            continue
        m = None if fence else HEADING.match(line)
        if m:
            out.append(m.group(1).replace("`", ""))
    return out


def page_anchors(path):
    ids, seen = set(), {}
    for h in heading_text(path):
        s = slug(h)
        k = seen.get(s, 0)
        ids.add(s if k == 0 else f"{s}-{k}")
        seen[s] = k + 1
    return ids


def pages():
    for d, _, names in os.walk(DOCS):
        for n in sorted(names):
            if n.endswith(".md"):
                yield os.path.join(d, n)


def check_summary(bad):
    with open(SUMMARY, encoding="utf-8") as fh:
        entries = [m.group(1) for m in LINK.finditer(fh.read())]
    listed = set()
    for e in entries:
        p = os.path.normpath(os.path.join(DOCS, e))
        listed.add(p)
        if not os.path.isfile(p):
            bad.append(f"docs/SUMMARY.md: entry {e} does not exist")
    for p in pages():
        if p != SUMMARY and p not in listed:
            bad.append(f"{os.path.relpath(p, ROOT)}: not in docs/SUMMARY.md")
    parts = [e for e in entries if e in PARTS]
    if parts != PARTS:
        bad.append(f"docs/SUMMARY.md: the six parts are {parts}, want {PARTS}")


def check_links(bad):
    for path in pages():
        rel = os.path.relpath(path, ROOT)
        with open(path, encoding="utf-8") as fh:
            lines = fh.read().splitlines()
        for n, line in strip_code(lines):
            for m in LINK.finditer(line):
                target = m.group(1)
                if re.match(r"^[a-z][a-z0-9+.-]*:", target):
                    continue
                file_part, _, anchor = target.partition("#")
                dest = path if not file_part else os.path.normpath(
                    os.path.join(os.path.dirname(path), file_part))
                if not os.path.exists(dest):
                    bad.append(f"{rel}:{n}: broken link {target}")
                elif anchor and dest.endswith(".md") and anchor not in page_anchors(dest):
                    bad.append(f"{rel}:{n}: no heading #{anchor} in {os.path.relpath(dest, ROOT)}")


def main():
    bad = []
    check_summary(bad)
    check_links(bad)
    for b in bad:
        print(b, file=sys.stderr)
    if not bad:
        print(f"check-book: ok ({sum(1 for _ in pages())} pages)")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
