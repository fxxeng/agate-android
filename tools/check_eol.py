#!/usr/bin/env python3
"""行尾体检：发现「只是行尾变了」的改动（整文件 churn），并审计属性名单。

用法：
    python3 tools/check_eol.py                   # 查工作区未提交的改动（提交前）
    python3 tools/check_eol.py --fix             # 按 blob 行尾改写工作区
    python3 tools/check_eol.py --commit HEAD     # 查某个提交引入的 churn（CI）
    python3 tools/check_eol.py --commit A..B
    python3 tools/check_eol.py --commit HEAD --fix   # 按父提交的行尾改写并暂存
    python3 tools/check_eol.py --audit           # 审计 .gitattributes 的 -text 名单

── 真正的原因（2026-11-06 用隔离仓库逐场景验证过）───────────────
本仓同一棵树里 CRLF 与 LF 两种 blob 共存（34 个 CRLF，0 个混合）。
`.gitattributes` 写了 `* text=auto eol=lf`，但它**只管入库那一刻**：
历史 blob 不会被回改，属性出现之后也不再重写它们。

于是这 34 个文件成了「规则与实际不符」的一批：

  · 工作区是 CRLF、blob 也是 CRLF，看起来一切正常；
  · 但只要改一次（哪怕只改一行），`add` 就按 `eol=lf` 把它转成 LF，
    产生整文件 churn（1461/1461 那种），而**本地编译与测试全绿**；
  · 本地 `core.autocrlf`（PortableGit 系统级默认 true）让它更不可预测。

2026-11 已经在这上面栽过三次（`http.rs`、客户端 4 个文件、`request.rs`），
每次都是「编译通过、测试全绿」之后才在 `git diff --numstat` 里发现。
人靠肉眼看不出来 —— 必须由机器判。

── 两层防线（分工不同，缺一不可）───────────────────────────────
1. **`.gitattributes` 给那 34 个历史 CRLF 文件标 `-text`**。
   实测清楚它的边界：**工作区保持 CRLF 时**，git 不再按 `eol=lf` 折成 LF，
   改一行就是 1/1（加属性前是 1461/1461）；但**工作区被工具整体改成 LF 时**，
   它照样如实记录 1461/1461 —— 属性拦不住工具的错误。
2. **本脚本**：CI 上拦「已经混进来的 churn」，本地能改回来，`--audit` 还能
   发现属性名单的漏网与过期。

换句话说：属性层保证「正常编辑不会被悄悄改写」，检查层保证「工具出错不会
静悄悄溜进提交」。只做一层都堵不住这一类问题。

── 判据（两级，都有实测依据）───────────────────────────────────
设 `plain` = 普通 diff 的行数（增+删），`real` = 加上
`--ignore-cr-at-eol` 之后还剩的行数。

  · `real == 0` 且 `plain > 0` ⇒ **纯行尾 churn**（内容一字未改）。
    实测：CRLF 文件整体转 LF ⇒ plain=1461、real=0。
  · `real / plain < 0.5` 且 `plain >= 20` ⇒ **行尾噪音掩盖了真实改动**。
    实测：改 1 行内容 + 整体转 LF ⇒ plain=25/25、real=1/1（掩掉 96%）。
    这种提交 review 时**看不出改了什么**，危害与纯 churn 同级。

第二级是必须的 —— 只判 `real == 0` 会漏掉最常见的事故形状：脚本既改了
内容、又顺手把整个文件的行尾换了。第一版就漏了这一类。

阈值 0.5 / 20 行是校准过的：翻遍近 20 个提交，没有任何文件的
`real / plain` 落在 0.6 以下 —— 真实开发不会产生「大 diff 里只藏几行改动」
这种形状，出现即异常。

反过来**不能**用「改动行数 ≈ 文件行数」当判据：新文件、大重写也会命中那个
形状（实测 `ImageOps.kt` 225/241、`images.rs` 303/278 都是真实重写）。
必须看「忽略 CR 之后还剩多少」。
"""
import argparse
import subprocess
import sys

# 真实改动不到 diff 规模的一半、且 diff 有足够行数 ⇒ 行尾噪音占了主导。
# 20 行下限是为了不误伤「改 3 行里有 2 行是 CRLF 噪音」这种小文件正常编辑。
COVER_RATIO = 0.5
MIN_LINES = 20


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


def unify(raw: bytes, kind: str) -> bytes:
    """把内容统一成指定行尾（先归一成 LF，再按需转 CRLF）。"""
    text = raw.replace(b"\r\n", b"\n")
    if kind == "CRLF":
        text = text.replace(b"\n", b"\r\n")
    return text


def run(*args: str) -> str:
    # `core.quotepath=false` 必须带：默认会把非 ASCII 路径写成
    # `"docs/\344\270\212..."`（八进制 + 引号），拿它当 pathspec 去查单个文件
    # 会**静默匹配到零个文件** —— 于是「忽略 CR 后的 diff」退化成对整个提交
    # 做 diff，每条都被误报成 churn。这个坑第一次写就踩到了。
    return subprocess.run(
        ["git", "-c", "core.quotepath=false", *args],
        capture_output=True,
        text=True,
        encoding="utf-8",
    ).stdout


def run_bytes(*args: str) -> bytes:
    return subprocess.run(args, capture_output=True).stdout


# ── 共用：把 `git diff --numstat` 读成 {路径: (增, 删)} ──────────────
def numstat(ignore_cr: bool, *spec: str) -> dict[str, tuple[int, int]]:
    """`{路径: (增, 删)}`；二进制文件记 `(-1, -1)`（调用方据此跳过）。

    `spec` 为空 = 工作区 vs 索引；给 `A..B` = 查提交范围。
    """
    args = ["diff", "--numstat", "--no-renames"]
    if ignore_cr:
        args.append("--ignore-cr-at-eol")
    args.extend(spec)
    result: dict[str, tuple[int, int]] = {}
    for line in run(*args).strip().splitlines():
        parts = line.split("\t")
        if len(parts) != 3:
            continue
        added, deleted, path = parts
        if added == "-" or deleted == "-":
            result[path] = (-1, -1)
            continue
        result[path] = (int(added), int(deleted))
    return result


def worktree_numstat(ignore_cr: bool) -> dict[str, tuple[int, int]]:
    """工作区 vs HEAD 的实际差异（已暂存 + 未暂存，合并起来）。

    合并两次查询而不是只看 `--cached`：提交前文件通常还没 `git add`，
    只看暂存区会漏掉全部改动。
    """
    merged: dict[str, tuple[int, int]] = {}
    for spec in (["--cached"], []):
        for path, (added, deleted) in numstat(ignore_cr, *spec).items():
            prev_add, prev_del = merged.get(path, (0, 0))
            merged[path] = (
                max(prev_add, 0) + max(added, 0),
                max(prev_del, 0) + max(deleted, 0),
            )
    return merged


# ── 判据 ────────────────────────────────────────────────────────
def classify(plain: dict, real: dict) -> list[tuple[str, int, int, int, str]]:
    """按两级判据挑出可疑文件：[(路径, 增, 删, 真实改动行数, 类型)]。

    `type`：`"pure"` = 纯行尾 churn；`"masked"` = 行尾噪音掩盖真实改动。
    """
    suspects = []
    for path, (added, deleted) in plain.items():
        if added < 0 or deleted < 0:
            continue  # 二进制，行尾概念不适用
        total = added + deleted
        if total == 0:
            continue
        got = real.get(path)
        remaining = 0 if got is None else sum(max(value, 0) for value in got)
        if remaining == 0:
            suspects.append((path, added, deleted, 0, "pure"))
        elif total >= MIN_LINES and remaining / total < COVER_RATIO:
            suspects.append((path, added, deleted, remaining, "masked"))
    return suspects


def report(suspects: list[tuple[str, int, int, int, str]], where: str) -> None:
    pure = [entry for entry in suspects if entry[4] == "pure"]
    masked = [entry for entry in suspects if entry[4] == "masked"]
    print(f"{where} 有 {len(suspects)} 个文件的行尾噪音占了主导：")
    if pure:
        print("  纯行尾（内容一字未改）：")
        for path, added, deleted, _, _ in pure:
            print(f"    · +{added}/-{deleted}  {path}")
    if masked:
        print("  行尾噪音掩盖了真实改动（review 时看不出改了什么）：")
        for path, added, deleted, remaining, _ in masked:
            hidden = added + deleted - remaining
            print(
                f"    · +{added}/-{deleted}  {path}"
                f"  真实改动仅 {remaining} 行，被 {hidden} 行行尾噪音盖住"
            )


# ── 模式一：工作区未提交的改动（提交前用）──────────────────────────
def check_worktree(fix: bool) -> int:
    """与提交模式用同一个两级判据（见 `classify`）。

    刻意不比「工作区字节 vs `HEAD:path` 字节」：本机 `core.autocrlf=true` 会让
    工作区在检出时就是 CRLF、而 blob 是 LF（或反之），两者天然不同 —— 按字节
    比会把每个文件都报成 DRIFT，全是噪音。diff 才是 git 自己的口径：它已经
    把 autocrlf / 属性都算进去了。
    """
    plain = worktree_numstat(ignore_cr=False)
    if not plain:
        print("工作区没有未提交的改动。")
        return 0
    bad = classify(plain, worktree_numstat(ignore_cr=True))

    if not bad:
        print(f"工作区 {len(plain)} 个改动文件都含实质内容，没有行尾噪音。")
        return 0

    report(bad, "工作区")
    if not fix:
        print("\n提交前应先修正：`python3 tools/check_eol.py --fix`")
        return 1

    for path, _, _, _, _ in bad:
        blob = subprocess.run(
            ["git", "cat-file", "-p", f"HEAD:{path}"], capture_output=True
        )
        if blob.returncode != 0:
            print(f"  SKIP   {path}（HEAD 里没有这个文件）")
            continue
        try:
            current = open(path, "rb").read()
        except OSError:
            print(f"  SKIP   {path}（不可读）")
            continue
        want = eol_kind(blob.stdout)
        with open(path, "wb") as handle:
            handle.write(unify(current, want))
        print(f"  fixed -> {want}  {path}")

    # 复验「噪音是否消失」，不是「差异是否消失」：真实改动本来就该留着。
    still = [entry[0] for entry in classify(
        worktree_numstat(ignore_cr=False), worktree_numstat(ignore_cr=True)
    )]
    still = [path for path in still if any(path == entry[0] for entry in bad)]
    if still:
        print("\n⚠️ 修复后行尾噪音仍在：")
        for path in still:
            print(f"  · {path}")
        return 1
    print("\n修复后复验通过：这些文件已不含行尾噪音。")
    return 0


# ── 模式二：提交引入的 churn（CI / pre-push 用）───────────────────
def commit_range(spec: str) -> str:
    """把「一个提交」或「范围」翻译成 `git diff` 能吃的**范围**表达式。

    ⚠️ `git diff HEAD` 的语义是「**工作区** vs HEAD」，不是「HEAD 改了什么」。
    这个错法很隐蔽：脚本会一直返回「没有文件改动」（因为 checkout 出来的
    工作区本来就与 HEAD 一致），于是 CI 永远绿 —— 拦不住任何东西。
    单提交必须写成 `HEAD^..HEAD`。
    """
    if ".." in spec:
        return spec
    return f"{spec}^..{spec}"


def check_commit(spec: str, fix: bool) -> int:
    rng = commit_range(spec)
    plain = numstat(False, rng)
    if not plain:
        print(f"{spec} 没有文件改动。")
        return 0
    # 关键：**一次性**对整个范围做忽略 CR 的 diff，再按路径取交集。
    # 不要逐文件 `git diff … -- path`：每个文件都要起一次 git 进程，而且路径
    # 一旦不匹配就静默退化成全量 diff（见 `run` 的注释）。
    bad = classify(plain, numstat(True, rng))

    if not bad:
        print(f"{spec}：没有行尾噪音（{len(plain)} 个改动文件都含实质内容）。")
        return 0

    report(bad, spec)
    if not fix:
        print(
            "\n这些改动应当撤回并按其原本的行尾重写。\n"
            f"修复：`python3 tools/check_eol.py --commit {spec} --fix`"
        )
        return 1

    base = spec.split("..")[0] if ".." in spec else f"{spec}^"
    for path, _, _, _, _ in bad:
        parent = run_bytes("git", "cat-file", "-p", f"{base}:{path}")
        if not parent:
            print(f"  SKIP   {path}（父侧没有这个文件）")
            continue
        try:
            current = open(path, "rb").read()
        except OSError:
            print(f"  SKIP   {path}（不可读）")
            continue
        want = eol_kind(parent)
        with open(path, "wb") as handle:
            handle.write(unify(current, want))
        # 属性层（2026-11-06 起）已经给这批文件标了 `-text`，正常情况下 `add`
        # 不会改行尾。但本机 `core.autocrlf=true` 仍可能插一脚，所以带上
        # `core.autocrlf=false` 把这条路也堵死。
        run("-c", "core.autocrlf=false", "add", "--", path)
        print(f"  fixed -> {want}  {path}（已暂存）")

    # 复验「噪音是否消失」，不是「差异是否消失」（同 `check_worktree`）。
    # 比较的是 `base` → **索引**：`--fix` 已经把修好的内容 `add` 进去了，
    # 索引就是「下次提交会写进仓库的东西」，是这里唯一有意义的检查对象。
    still = [entry[0] for entry in classify(
        numstat(False, base), numstat(True, base)
    )]
    still = [path for path in still if any(path == entry[0] for entry in bad)]
    if still:
        print("\n⚠️ 修复后行尾噪音仍在 —— `add` 又把行尾折回去了。")
        print(
            "  根因：文件没被标成 `-text`，`eol=lf` 属性在 add 时强制转换，\n"
            "  连 `-c core.autocrlf=false` 都拦不住（实测）。\n"
            "  解法：把该文件加进 `.gitattributes` 的 `-text` 名单（见模块头）。"
        )
        for path in still:
            print(f"  · {path} -text")
        return 1
    print("\n修复后复验通过：这些文件已不含行尾噪音。")
    print(
        f"注意两点：\n"
        f"  1. 修的是**工作区**。重新提交后 `--commit {spec}` 才会变干净。\n"
        f"  2. 如果你是在**修复一条已经存在的 churn 提交**（先 churn、再修），\n"
        f"     那么修完那个提交本身仍会报一次 —— 它相对父提交确实是「整文件\n"
        f"     替换」。要确认整体已经回正，用范围查：\n"
        f"       python3 tools/check_eol.py --commit {base}..HEAD"
    )
    return 0


# ── 模式三：审计 `.gitattributes` 的 `-text` 名单 ──────────────────
def audit_attributes() -> int:
    """检查 `-text` 名单与仓库实际 CRLF blob 是否一致。

    名单是**快照**：那 34 行是 2026-11 一次性补的，之后新文件都走 `eol=lf`。
    随着时间推移会出现两种偏差，两种都要能发现：

      · **名单过期**（文件被重新归一成 LF，名单里还留着）—— 无害但误导，
        下一个人会以为「这个文件是 CRLF」；
      · **有漏网**（仓库里有 CRLF blob 却没进名单）—— 危险，它下次被编辑时
        就会 churn，而这正是要根除的东西。

    只看 blob 的真实字节（`cat-file`），不看工作区 —— 工作区受 autocrlf 影响，
    不可靠（本轮开头就据此纠正过记忆里一份错误的文件清单）。
    """
    listed: set[str] = set()
    try:
        with open(".gitattributes", encoding="utf-8") as handle:
            for line in handle:
                line = line.strip()
                if not line or line.startswith("#"):
                    continue
                parts = line.split()
                if len(parts) >= 2 and parts[-1] == "-text":
                    listed.add(parts[0])
    except OSError:
        print("读不到 .gitattributes")
        return 1

    actual: set[str] = set()
    out = subprocess.run(["git", "ls-files", "-z"], capture_output=True).stdout
    for raw in out.split(b"\0"):
        if not raw:
            continue
        path = raw.decode("utf-8")
        blob = subprocess.run(
            ["git", "cat-file", "-p", f"HEAD:{path}"], capture_output=True
        ).stdout
        if not blob or b"\0" in blob[:4096]:
            continue
        crlf, lf = blob.count(b"\r\n"), blob.count(b"\n")
        if lf and crlf == lf:
            actual.add(path)

    missing = sorted(actual - listed)
    stale = sorted(listed - actual)
    if not missing and not stale:
        print(f"名单与实际一致（{len(actual)} 个 CRLF 文件）。")
        return 0

    if missing:
        print(f"⚠️ 有 {len(missing)} 个 CRLF 文件**没进** `-text` 名单（下次编辑会 churn）：")
        for path in missing:
            print(f"  {path} -text")
    if stale:
        print(f"\n· 有 {len(stale)} 个名单条目已不是 CRLF（可删，留着会误导）：")
        for path in stale:
            print(f"  · {path}")
    return 1


def main() -> int:
    parser = argparse.ArgumentParser(
        description="行尾体检：发现整文件 churn，并审计 -text 名单",
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument("--fix", action="store_true", help="按原有行尾改写")
    parser.add_argument(
        "--commit",
        metavar="SPEC",
        help="查某个提交 / 范围引入的行尾 churn（默认查工作区）",
    )
    parser.add_argument(
        "--audit",
        action="store_true",
        help="审计 .gitattributes 的 -text 名单是否与实际 CRLF 文件一致",
    )
    args = parser.parse_args()
    if args.audit:
        return audit_attributes()
    if args.commit:
        return check_commit(args.commit, args.fix)
    return check_worktree(args.fix)


if __name__ == "__main__":
    sys.exit(main())