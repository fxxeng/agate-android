"""Rust 源文件括号平衡静态检查（无 cargo 环境下的替代验证手段）。

用途：本机没有 Rust 工具链，内核改动无法编译验证。这个脚本按 Rust 词法
规则（行注释 / 可嵌套块注释 / r#"..."# 原始字符串 / 转义 / 字符字面量）
剥离非代码内容后统计 (){}[] 的平衡，用来抓「编辑时漏了大括号」这类硬伤。

局限：只查配平，不做类型检查。函数签名、字段名、类型是否对得上仍需人工
对着源码核 —— 脚本不能替代 cargo check。
"""
import sys
from pathlib import Path


def scan(path: Path):
    s = path.read_text(encoding="utf-8")
    i, n = 0, len(s)
    brace = paren = bracket = 0
    line = 1
    problems = []
    while i < n:
        c = s[i]
        if c == "\n":
            line += 1
            i += 1
            continue
        # 行注释
        if c == "/" and i + 1 < n and s[i + 1] == "/":
            while i < n and s[i] != "\n":
                i += 1
            continue
        # 块注释（Rust 允许嵌套）
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
        # 原始字符串 r"..." / r#"..."# / r##"..."##
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
                    problems.append("line %d: 未闭合的原始字符串" % line)
                    return None, None, None, problems
                line += s.count("\n", i, k + len(closer))
                i = k + len(closer)
                continue
        # 普通字符串
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
        # 字符字面量 'x' / '\n' / 以及生命周期（'a 不配对）
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
            if brace < 0:
                problems.append("line %d: 多余的 }" % line)
        elif c == "(":
            paren += 1
        elif c == ")":
            paren -= 1
            if paren < 0:
                problems.append("line %d: 多余的 )" % line)
        elif c == "[":
            bracket += 1
        elif c == "]":
            bracket -= 1
            if bracket < 0:
                problems.append("line %d: 多余的 ]" % line)
        i += 1
    return brace, paren, bracket, problems


def main(argv):
    targets = []
    for raw in argv[1:]:
        p = Path(raw)
        if p.is_dir():
            targets.extend(sorted(p.rglob("*.rs")))
        else:
            targets.append(p)
    bad = 0
    for path in targets:
        brace, paren, bracket, problems = scan(path)
        if brace is None:
            print("%-46s SCAN FAILED: %s" % (path, problems))
            bad += 1
            continue
        ok = brace == 0 and paren == 0 and bracket == 0 and not problems
        if ok:
            print("%-46s OK" % path)
        else:
            print("%-46s brace=%+d paren=%+d bracket=%+d %s"
                  % (path, brace, paren, bracket, problems))
            bad += 1
    print("\n%d file(s), %d problem(s)" % (len(targets), bad))
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))