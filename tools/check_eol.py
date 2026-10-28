#!/usr/bin/env python3
"""提交前体检：列出待提交文件的行尾构成，并标出「修改了但行尾与远端 blob 不一致」的文件。

用法：
    python3 tools/check_eol.py            # 只报告
    python3 tools/check_eol.py --fix      # 把行尾统一成远端 blob 的类型后重写

为什么要单独一个脚本：本仓 .gitattributes 写了 `* text=auto eol=lf`，但历史 blob
从未归一——远端同一棵树里既有 CRLF 文件也有 LF 文件。改任何一个已存在文件时若
不按它原本的行尾写回，就会产生**整文件 churn**（git diff 显示几千行改动）。
这个脚本把那次逐文件判断固化成可复用的检查。
"""
import subprocess
import sys


def eol_kind(raw: bytes) -> str:
    crlf = raw.count(b"\r\n")
    lf = raw.count(b"\n")
    if lf == 0:
        return "EMPTY"
    if crlf == lf:
        return "CRLF"
    if crlf == 0:
        return "LF"
    return "MIXED"


def remote_blob(path: str) -> bytes | None:
    """取该路径在 HEAD 里的原始 blob；新文件返回 None。"""
    r = subprocess.run(
        ["git", "cat-file", "-p", f"HEAD:{path}"],
        capture_output=True,
    )
    return r.stdout if r.returncode == 0 else None


def main() -> int:
    fix = "--fix" in sys.argv

    # -z 用 NUL 分隔且**不做路径转义**：默认的 porcelain 会把中文路径写成
    # "docs/\344\273\273..." 这种八进制形式，直接当文件名用会打不开。
    out = subprocess.run(
        ["git", "status", "--porcelain", "-z"],
        capture_output=True, text=True, encoding="utf-8",
    ).stdout
    paths = []
    for entry in out.split("\0"):
        if not entry.strip():
            continue
        path = entry[3:]
        paths.append(path)

    bad = []
    for path in paths:
        try:
            with open(path, "rb") as f:
                cur = f.read()
        except OSError:
            print(f"  SKIP   {path}（已删除或不可读）")
            continue

        ck = eol_kind(cur)
        rb = remote_blob(path)
        if rb is None:
            print(f"  NEW    {ck:5}  {path}")
            continue

        rk = eol_kind(rb)
        mark = "OK    " if ck == rk else "DRIFT "
        print(f"  {mark} {rk:5} -> {ck:5}  {path}")
        if ck != rk:
            bad.append((path, rk, cur))

    if not bad:
        print("\n行尾全部与远端一致。")
        return 0

    print(f"\n{len(bad)} 个文件行尾漂移：")
    for path, _, _ in bad:
        print(f"  · {path}")

    if not fix:
        print("\n加 --fix 按远端行尾统一重写。")
        return 1

    for path, rk, cur in bad:
        text = cur.decode("utf-8")
        unified = text.replace("\r\n", "\n")
        if rk == "CRLF":
            unified = unified.replace("\n", "\r\n")
        with open(path, "wb") as f:
            f.write(unified.encode("utf-8"))
        print(f"  fixed -> {rk}  {path}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
