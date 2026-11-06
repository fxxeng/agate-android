#!/usr/bin/env python3
"""`tools/check_eol.py` 的判据单测（纯逻辑，不碰 git）。

跑法：
    python3 tools/test_check_eol.py

── 为什么值得单独测 ────────────────────────────────────────────
`classify()` 是整条防线的判官：它判错的方向有两个，代价都是「让人不再信任它」：
  · **假阳性**（把真实改动报成 churn）⇒ 每次提交都被拦，人就会习惯性忽略它；
  · **假阴性**（漏掉真 churn）⇒ 事故照样溜进仓库，白写这个脚本。
历史上第一版就漏了「改内容 + 换行尾」这种最常见形状（只判 `real == 0`），
所以这里把四种边界都钉住。
"""
import sys
import unittest

sys.path.insert(0, __file__.rsplit("/", 2)[0] if "/" in __file__ else ".")
sys.path.insert(0, "tools")

from check_eol import COVER_RATIO, MIN_LINES, classify, eol_kind, unify  # noqa: E402


class ClassifyTest(unittest.TestCase):
    def test_real_edit_is_not_flagged(self):
        """单纯的内容改动：忽略 CR 后还剩一样多 ⇒ 不该报。"""
        self.assertEqual(classify({"a.rs": (3, 3)}, {"a.rs": (3, 3)}), [])

    def test_pure_churn_is_flagged(self):
        """整体换行尾：忽略 CR 后归零 ⇒ pure。"""
        result = classify({"a.rs": (1461, 1461)}, {})
        self.assertEqual(len(result), 1)
        self.assertEqual(result[0][4], "pure")

    def test_churn_that_vanishes_entirely(self):
        """忽略 CR 后该文件从 diff 里消失（`real` 里没有这条）。"""
        self.assertEqual(classify({"a.rs": (5, 5)}, {})[0][4], "pure")

    def test_masked_edit_is_flagged(self):
        """改 1 行 + 整体换行尾（25/25 里只有 1/1 是真改动）⇒ masked。

        这是第一版**漏掉**的形状，也是最常见的：脚本既改内容又顺手换了行尾。
        """
        result = classify({"a.rs": (25, 25)}, {"a.rs": (1, 1)})
        self.assertEqual(len(result), 1)
        self.assertEqual(result[0][3], 2)
        self.assertEqual(result[0][4], "masked")

    def test_small_file_is_not_flagged(self):
        """小文件里行尾占多数也不报：那样会误伤正常的小编辑。

        `MIN_LINES` 是 20，所以 3 行的文件里哪怕 2/3 都是行尾噪音也不报 ——
        这是刻意的：小文件改动本来就少，报出来的噪音会让人关掉这个检查。
        """
        self.assertEqual(classify({"a.rs": (3, 3)}, {"a.rs": (2, 2)}), [])
        # 边界另一侧：够 20 行、且真实改动不足一半 ⇒ 要报
        flagged = classify({"b.rs": (20, 20)}, {"b.rs": (4, 4)})
        self.assertEqual(len(flagged), 1, "够长且大多数是噪音，应当报")

    def test_threshold_boundary(self):
        """刚好到阈值的判据（防止把 `<` 写成 `<=` 或反过来）。"""
        total = MIN_LINES
        just_under = int(total * COVER_RATIO) - 1
        result = classify({"a.rs": (total // 2, total - total // 2)}, {
            "a.rs": (just_under // 2, just_under - just_under // 2)
        })
        self.assertEqual(len(result), 1, "低于阈值应报")

    def test_binary_is_skipped(self):
        """二进制（numstat 记 `-`）不参与行尾判定。"""
        self.assertEqual(classify({"a.png": (-1, -1)}, {}), [])

    def test_new_file_is_not_flagged(self):
        """纯新增文件没有「原来的行尾」可比，不报。"""
        self.assertEqual(classify({"new.rs": (400, 0)}, {"new.rs": (400, 0)}), [])

    def test_multiple_files_are_evaluated_independently(self):
        """同一批改动里，只有可疑的那几个被挑出来。"""
        result = classify(
            {"good.rs": (10, 10), "bad.rs": (100, 100)},
            {"good.rs": (10, 10)},
        )
        self.assertEqual([entry[0] for entry in result], ["bad.rs"])


class EolKindTest(unittest.TestCase):
    def test_crlf(self):
        self.assertEqual(eol_kind(b"a\r\nb\r\n"), "CRLF")

    def test_lf(self):
        self.assertEqual(eol_kind(b"a\nb\n"), "LF")

    def test_mixed(self):
        self.assertEqual(eol_kind(b"a\r\nb\n"), "MIXED")

    def test_empty(self):
        self.assertEqual(eol_kind(b""), "EMPTY")

    def test_unify_to_crlf_is_idempotent(self):
        raw = b"a\r\nb\r\n"
        self.assertEqual(unify(raw, "CRLF"), raw)
        self.assertEqual(unify(unify(raw, "CRLF"), "CRLF"), raw)

    def test_unify_to_lf_from_mixed(self):
        self.assertEqual(unify(b"a\r\nb\nc", "LF"), b"a\nb\nc")

    def test_unify_does_not_touch_lone_cr(self):
        """孤立 CR（不是换行）不能被当成行尾 —— 否则会改坏内容。"""
        self.assertEqual(unify(b"a\rb\n", "LF"), b"a\rb\n")


if __name__ == "__main__":
    unittest.main(verbosity=2)