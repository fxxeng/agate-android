"""定位 Rust 文件里括号深度的偏移起点（给 rust_balance.py 的报告做下钻）。"""
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from rust_balance import scan  # noqa: E402


def walk(path: Path):
    s = path.read_text(encoding="utf-8")
    i, n = 0, len(s)
    line = 1
    brace = paren = bracket = 0
    rows = []
    while i < n:
        c = s[i]
        if c == "\n":
            rows.append((line, brace, paren, bracket))
            line += 1
            i += 1
            continue
        if c == "/" and i + 1 < n and s[i + 1] == "/":
            while i < n and s[i] != "\n":
                i += 1
            continue
        if c == "/" and i + 1 < n and s[i + 1] == "*":
            depth = 1
            i += 2
            while i < n and depth:
                if s[i] == "/" and i + 1 < n and s[i + 1] == "*":
                    depth += 1
                    i += 2
                    continue
                if s[i] == "*" and i + 1 < n and s[i + 1] == "/":
                    depth -= 1
                    i += 2
                    continue
                if s[i] == "\n":
                    line += 1
                i += 1
            continue
        if c == "r" and i + 1 < n and s[i + 1] in '#"':
            j = i + 1
            hashes = 0
            while j < n and s[j] == "#":
                hashes += 1
                j += 1
            if j < n and s[j] == '"':
                closer = '"' + "#" * hashes
                k = s.find(closer, j + 1)
                if k == -1:
                    print("未闭合原始字符串 @ line", line)
                    return
                line += s.count("\n", i, k + len(closer))
                i = k + len(closer)
                continue
        if c == '"':
            i += 1
            while i < n:
                if s[i] == "\\":
                    i += 2
                    continue
                if s[i] == '"':
                    i += 1
                    break
                if s[i] == "\n":
                    line += 1
                i += 1
            continue
        if c == "'":
            if i + 2 < n and s[i + 1] == "\\":
                j = s.find("'", i + 2)
                if j != -1 and j - i <= 6:
                    i = j + 1
                    continue
            elif i + 2 < n and s[i + 2] == "'":
                i += 3
                continue
            else:
                i += 1
                continue
        if c == "{":
            brace += 1
        elif c == "}":
            brace -= 1
        elif c == "(":
            paren += 1
        elif c == ")":
            paren -= 1
        elif c == "[":
            bracket += 1
        elif c == "]":
            bracket -= 1
        i += 1

    src = s.split("\n")
    prev = None
    for (ln, br, pa, bk) in rows:
        key = (br, pa, bk)
        if key != prev:
            text = src[ln - 1] if ln - 1 < len(src) else ""
            print("L%-4d brace=%+d paren=%+d bracket=%+d | %s"
                  % (ln, br, pa, bk, text.strip()[:74]))
            prev = key


if __name__ == "__main__":
    walk(Path(sys.argv[1]))