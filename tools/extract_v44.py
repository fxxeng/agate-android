#!/usr/bin/env python3
"""从 V44 设计基线里抽出二级页的分组标题与卡片标题，供方案摸排使用。"""
import re
import sys

SRC = "docs/Agate-UI设计基线-v44.html"


def main() -> int:
    text = open(SRC, encoding="utf-8").read()
    spans = [(m.start(), m.group(1)) for m in re.finditer(r'id="(sub-[^"]+)"', text)]
    if not spans:
        print("no sub pages found")
        return 1
    for i, (start, sid) in enumerate(spans):
        end = spans[i + 1][0] if i + 1 < len(spans) else len(text)
        seg = text[start:end]
        print("=" * 60)
        print("PAGE", sid, f"({end - start} chars)")
        for m in re.finditer(r'class="sg-t">([^<]+)<', seg):
            print("   group:", m.group(1).strip())
        for m in re.finditer(r'class="card-title[^"]*">([^<]+)<', seg):
            t = re.sub(r"<[^>]+>", "", m.group(1)).strip()
            if t:
                print("   title:", t)
        for m in re.finditer(r'class="ptab[^"]*"[^>]*>([^<]+)<', seg):
            print("   ptab :", m.group(1).strip())
    return 0


if __name__ == "__main__":
    sys.exit(main())
