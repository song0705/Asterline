#!/usr/bin/env python3
"""Align GitHub-Flavored Markdown table pipes by terminal display width."""

from __future__ import annotations

import re
import sys
import unicodedata
from pathlib import Path


SEPARATOR = re.compile(r"^:?-{3,}:?$")


def display_width(text: str) -> int:
    return sum(
        0
        if unicodedata.combining(char)
        else 2
        if unicodedata.east_asian_width(char) in {"W", "F"}
        else 1
        for char in text
    )


def split_row(line: str) -> list[str]:
    cells: list[str] = []
    current: list[str] = []
    escaped = False
    for char in line.strip()[1:-1]:
        if char == "|" and not escaped:
            cells.append("".join(current).strip())
            current = []
        else:
            current.append(char)
        escaped = char == "\\" and not escaped
        if char != "\\":
            escaped = False
    cells.append("".join(current).strip())
    return cells


def format_block(lines: list[str]) -> list[str]:
    rows = [split_row(line) for line in lines]
    if len(rows) < 2 or len({len(row) for row in rows}) != 1:
        return lines

    widths = [
        max(3, *(display_width(row[column]) for row in rows if not SEPARATOR.fullmatch(row[column])))
        for column in range(len(rows[0]))
    ]

    formatted: list[str] = []
    for row in rows:
        cells: list[str] = []
        for cell, width in zip(row, widths):
            if SEPARATOR.fullmatch(cell):
                left = ":" if cell.startswith(":") else ""
                right = ":" if cell.endswith(":") else ""
                cell = left + "-" * (width - len(left) - len(right)) + right
            padding = " " * (width - display_width(cell))
            cells.append(f" {cell}{padding} ")
        formatted.append("|" + "|".join(cells) + "|")
    return formatted


def format_file(path: Path) -> None:
    original = path.read_text()
    lines = original.splitlines()
    output: list[str] = []
    index = 0
    while index < len(lines):
        if lines[index].startswith("|") and lines[index].endswith("|"):
            end = index
            while end < len(lines) and lines[end].startswith("|") and lines[end].endswith("|"):
                end += 1
            output.extend(format_block(lines[index:end]))
            index = end
        else:
            output.append(lines[index])
            index += 1
    result = "\n".join(output) + ("\n" if original.endswith("\n") else "")
    if result != original:
        path.write_text(result)


def main() -> None:
    if len(sys.argv) < 2:
        raise SystemExit("usage: format_markdown_tables.py FILE...")
    for filename in sys.argv[1:]:
        format_file(Path(filename))


if __name__ == "__main__":
    main()
