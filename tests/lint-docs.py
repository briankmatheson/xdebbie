#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Lint the documentation (run by tests/lint-links.sh).

Checks README.md, CONTRIBUTING.md, SECURITY.md and docs/**/*.md for:
  - relative links whose file or #anchor does not exist;
  - code fences without a language tag;
  - <angle-bracket> placeholders in bash blocks (use UPPERCASE);
  - site-specific names that must not be published;
  - pages missing from docs/SUMMARY.md.
Exit status 1 on any finding.
"""
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
TOP = ["README.md", "CONTRIBUTING.md", "SECURITY.md"]
# Names of the author's own machines and domains: never in published docs.
SITE = re.compile(r"bmath\.nyc|\btimesink\b|\bgram\b|\baviary\b|mynetworksettings|\bnode0\b",
                  re.IGNORECASE)
LINK = re.compile(r"(?<!!)\[[^\]]*\]\(([^)\s]+)(?:\s+\"[^\"]*\")?\)")
PLACEHOLDER = re.compile(r"(?<![<\w])<[a-z][a-z0-9 _'.-]*>(?!>)")


def files():
    out = [os.path.join(ROOT, f) for f in TOP if os.path.exists(os.path.join(ROOT, f))]
    for d, _, names in os.walk(os.path.join(ROOT, "docs")):
        out += [os.path.join(d, n) for n in sorted(names) if n.endswith(".md")]
    return out


def slug(heading):
    """mdBook's (and, for ASCII, GitHub's) heading id."""
    h = re.sub(r"\[([^\]]*)\]\([^)]*\)", r"\1", heading)  # links keep their text
    h = h.strip().lower()
    out = []
    for c in h:
        if c.isalnum() or c in "_-":
            out.append(c)
        elif c.isspace():
            out.append("-")
    return "".join(out)


def anchors(path, cache={}):
    if path not in cache:
        ids, seen, fence = set(), {}, False
        with open(path, encoding="utf-8") as fh:
            for line in fh:
                if line.lstrip().startswith("```"):
                    fence = not fence
                    continue
                m = None if fence else re.match(r"^#{1,6}\s+(.*?)\s*#*\s*$", line)
                if m:
                    s = slug(m.group(1))
                    n = seen.get(s, 0)
                    ids.add(s if n == 0 else f"{s}-{n}")
                    seen[s] = n + 1
        cache[path] = ids
    return cache[path]


def main():
    bad = []
    for path in files():
        rel = os.path.relpath(path, ROOT)
        fence, lang = False, ""
        with open(path, encoding="utf-8") as fh:
            lines = fh.readlines()
        for n, line in enumerate(lines, 1):
            stripped = line.lstrip()
            if stripped.startswith("```"):
                if not fence:
                    fence, lang = True, stripped[3:].strip()
                    if not lang:
                        bad.append(f"{rel}:{n}: code block without a language")
                else:
                    fence = False
                continue
            if SITE.search(line):
                bad.append(f"{rel}:{n}: site-specific name: {line.strip()}")
            if fence:
                if lang in ("bash", "sh", "console"):
                    code = line.split("#", 1)[0] if lang != "console" else line
                    for m in PLACEHOLDER.finditer(code):
                        bad.append(f"{rel}:{n}: placeholder {m.group(0)}: write it in UPPERCASE")
                continue
            for m in LINK.finditer(line):
                target = m.group(1)
                if re.match(r"^[a-z]+:", target):
                    continue
                file_part, _, anchor = target.partition("#")
                dest = path if not file_part else os.path.normpath(
                    os.path.join(os.path.dirname(path), file_part))
                if not os.path.exists(dest):
                    bad.append(f"{rel}:{n}: broken link {target}")
                    continue
                if anchor and dest.endswith(".md") and anchor not in anchors(dest):
                    bad.append(f"{rel}:{n}: no heading #{anchor} in {os.path.relpath(dest, ROOT)}")

    summary = os.path.join(ROOT, "docs", "SUMMARY.md")
    if os.path.exists(summary):
        listed = set()
        with open(summary, encoding="utf-8") as fh:
            for m in LINK.finditer(fh.read()):
                listed.add(os.path.normpath(os.path.join(ROOT, "docs", m.group(1))))
        for path in files():
            if path.startswith(os.path.join(ROOT, "docs")) and path != summary and path not in listed:
                bad.append(f"{os.path.relpath(path, ROOT)}: not listed in docs/SUMMARY.md")

    for b in bad:
        print(b, file=sys.stderr)
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
