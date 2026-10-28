"""通过 GitHub Git Data API 推送一个本地提交（绕开 git 的 HTTPS 传输）。

背景：本机网络环境下 git 的 HTTPS 传输被代理拦死
（`CONNECT tunnel failed, response 502`），而 `api.github.com` 完全可用。
所以在本地用 git 造好提交后，改用 REST API 把同样的树与提交搬到远端：

  1. 对每个新增/修改的文件 POST /git/blobs（base64 内容，二进制安全）
  2. POST /git/trees，带 base_tree=远端当前树的 sha，只列本提交动的路径；
     删除的路径传 sha:null
  3. POST /git/commits，parent 用远端 ref 当前指向的 sha，
     author/committer 沿用本地提交里的身份与时间
  4. PATCH /git/refs/heads/<branch>，**不带 force** —— 如果远端在此期间
     前进，接口会报 422 而不是悄悄覆盖别人的提交

用法：
    python tools/api_push.py <token> <repo> <branch> <local_commit>
"""

import base64
import json
import subprocess
import sys
import urllib.error
import urllib.request

API = "https://api.github.com"


def git(*args, binary=False):
    r = subprocess.run(["git"] + list(args), capture_output=True)
    if r.returncode != 0:
        raise SystemExit(f"git {' '.join(args)} 失败:\n{r.stderr.decode('utf-8', 'replace')}")
    return r.stdout if binary else r.stdout.decode("utf-8").strip()


def call(token, method, path, payload=None):
    url = API + path
    data = json.dumps(payload).encode("utf-8") if payload is not None else None
    req = urllib.request.Request(url, data=data, method=method)
    req.add_header("Authorization", "Bearer " + token)
    req.add_header("Accept", "application/vnd.github+json")
    if data:
        req.add_header("Content-Type", "application/json")
    try:
        with urllib.request.urlopen(req, timeout=90) as r:
            body = r.read().decode("utf-8")
            return r.status, (json.loads(body) if body.strip() else {})
    except urllib.error.HTTPError as e:
        body = e.read().decode("utf-8", "replace")
        return e.code, {"error": body[:600]}


def parse_raw(diff_out):
    """解析 `git diff --raw -z`，返回 [(status, path), ...]。

    用 -z 是为了避免非 ASCII 路径被转义成 `"docs/\\344\\273..."` 这种形式 ——
    中文文件名在 .git 的默认输出里一定会被转义，转义后既不能当路径用，
    也很难可靠地反转义回来。
    """
    out = []
    # 记录格式（NUL 分隔）: ":oldmode newmode oldsha newsha status\0path\0"
    parts = diff_out.split("\0")
    i = 0
    while i < len(parts):
        head = parts[i]
        if not head.startswith(":"):
            i += 1
            continue
        status = head.split(" ")[-1]
        path = parts[i + 1]
        out.append((status, path))
        # 重命名/复制多带一个源路径段
        i += 3 if status[:1] in ("R", "C") else 2
    return out


def main():
    if len(sys.argv) < 5:
        raise SystemExit(__doc__)
    token, repo, branch, commit = sys.argv[1:5]

    parent = git("rev-parse", commit + "^")
    print(f"local  commit : {commit}")
    print(f"local  parent : {parent}")

    st, ref = call(token, "GET", f"/repos/{repo}/git/ref/heads/{branch}")
    if st != 200:
        raise SystemExit(f"读远端 ref 失败 [{st}]: {ref}")
    remote_head = ref["object"]["sha"]
    print(f"remote head   : {remote_head}")

    if remote_head == commit:
        print("远端已经指向该提交，无需推送。")
        return 0
    if remote_head != parent:
        raise SystemExit(
            f"基点不一致：远端在 {remote_head}，本地提交的父是 {parent}。\n"
            "说明远端有本地没有的提交，请先 fetch 并处理，不要强行覆盖。"
        )

    changes = parse_raw(git("diff", "--raw", "-z", parent, commit, binary=True).decode("utf-8"))
    print(f"changes       : {len(changes)} 个路径")

    tree = []
    for status, path in changes:
        code = status[0]
        if code == "D":
            tree.append({"path": path, "mode": "100644", "type": "blob", "sha": None})
            print(f"  D {path}")
            continue

        raw = git("show", f"{commit}:{path}", binary=True)
        st, blob = call(
            token,
            "POST",
            f"/repos/{repo}/git/blobs",
            {"content": base64.b64encode(raw).decode("ascii"), "encoding": "base64"},
        )
        if st != 201:
            raise SystemExit(f"上传 blob 失败 [{st}] {path}: {blob}")
        tree.append({"path": path, "mode": "100644", "type": "blob", "sha": blob["sha"]})
        print(f"  {code} {path}  -> {blob['sha'][:10]} ({len(raw)} bytes)")

    st, new_tree = call(
        token,
        "POST",
        f"/repos/{repo}/git/trees",
        {"base_tree": git("rev-parse", parent + "^{tree}"), "tree": tree},
    )
    if st != 201:
        raise SystemExit(f"建 tree 失败 [{st}]: {new_tree}")
    print(f"new tree      : {new_tree['sha']}")

    # 沿用本地提交里的作者/提交者与时间，保证远端提交的身份与本地一致
    fmt = "%an%x00%ae%x00%aI%x00%cn%x00%ce%x00%cI"
    an, ae, ai, cn, ce, ci = git("show", "-s", f"--format={fmt}", commit).split("\0")
    message = git("show", "-s", "--format=%B", commit)

    st, new_commit = call(
        token,
        "POST",
        f"/repos/{repo}/git/commits",
        {
            "message": message,
            "tree": new_tree["sha"],
            "parents": [remote_head],
            "author": {"name": an, "email": ae, "date": ai},
            "committer": {"name": cn, "email": ce, "date": ci},
        },
    )
    if st != 201:
        raise SystemExit(f"建 commit 失败 [{st}]: {new_commit}")
    print(f"new commit    : {new_commit['sha']}")

    st, updated = call(
        token,
        "PATCH",
        f"/repos/{repo}/git/refs/heads/{branch}",
        {"sha": new_commit["sha"], "force": False},
    )
    if st not in (200, 201):
        raise SystemExit(f"更新 ref 失败 [{st}]: {updated}")

    print(f"\n远端 {branch} 已更新: {remote_head} -> {new_commit['sha']}")
    print("注意：本地存储的 origin ref 还没变，稍后 `git fetch` 一次即可对齐。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
