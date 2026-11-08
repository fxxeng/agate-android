#!/usr/bin/env python3
"""Check that every private external method in Gateway.kt has a JNI export.

R47/T3.5 签名级校验（在既有符号名比对之上叠加）：
  · nm 符号名集合比对：.so 动态符号表是「导出是否存在」的唯一真相（保留）；
  · Kotlin/Rust 双端描述符互检（新增）：12 个入口均无重载，JVM 按符号名绑定
    native 方法 —— 参数类型/顺序漂移在 nm 层面完全不可见，只在运行期以垃圾
    参数 UB 形态爆发。双端各自机械解析出 JNI 方法描述符后互检，任一侧漂移
    即 fail；出现映射表外的类型也直接 fail（新契约必须先更新映射表，使契约
    变更在 CI 强制可见）。

JNI 方法描述符（method descriptor）只描述隐式接收者之后的参数与返回值：
  Kotlin `external fun nativeCoreInit(dataDir: String, port: Int, schedulerOn: Boolean): String?`
  → (Ljava/lang/String;IZ)Ljava/lang/String;
  Rust `(..., data_dir: JString, port: jint, scheduler_on: jboolean) -> jstring` 同。
"""
from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path

# Gateway.kt external fun 参数类型 → JNI 描述符（现有 12 入口类型的闭集）。
KOTLIN_PARAM_TO_DESCRIPTOR = {
    "String": "Ljava/lang/String;",
    "String?": "Ljava/lang/String;",
    "Int": "I",
    "Long": "J",
    "Boolean": "Z",
    "Any?": "Ljava/lang/Object;",
}
# 返回类型独立闭集：现网全部为 String?（信封回复）；nativeVersion 亦然。
KOTLIN_RETURN_TO_DESCRIPTOR = {
    "String?": "Ljava/lang/String;",
    "String": "Ljava/lang/String;",
}

# jni_bridge.rs extern "system" fn 类型 → JNI 描述符（同一闭集的 Rust 面）。
RUST_PARAM_TO_DESCRIPTOR = {
    "JString": "Ljava/lang/String;",
    "jint": "I",
    "jlong": "J",
    "jboolean": "Z",
    "JObject": "Ljava/lang/Object;",
}
RUST_RETURN_TO_DESCRIPTOR = {
    "jstring": "Ljava/lang/String;",
}

KOTLIN_DECL_RE = re.compile(
    r"private\s+external\s+fun\s+(\w+)\s*\(([^)]*)\)\s*:\s*([\w?]+)"
)
RUST_EXPORT_RE = re.compile(
    r'pub\s+extern\s+"system"\s+fn\s+(Java_dev_agate_core_Gateway_\w+)'
    r"(?:<[^>]*>)?\s*\((.*?)\)\s*->\s*(\w+)",
    re.DOTALL,
)


def fail(message: str) -> None:
    print(f"ERROR: {message}", file=sys.stderr)
    raise SystemExit(2)


def parse_kotlin(source: str) -> dict[str, str]:
    """Gateway.kt → {symbol: descriptor}；类型超出映射表即报错（契约变更必须显式进表）。"""
    methods: dict[str, str] = {}
    for match in KOTLIN_DECL_RE.finditer(source):
        name, raw_params, raw_return = match.groups()
        params = [p.strip() for p in raw_params.split(",") if p.strip()]
        parts = []
        for param in params:
            _, sep, param_type = param.rpartition(":")
            if not sep:
                fail(f"Kotlin 参数缺少类型标注：{name}({param})")
            param_type = param_type.strip()
            descriptor = KOTLIN_PARAM_TO_DESCRIPTOR.get(param_type)
            if descriptor is None:
                fail(
                    f"Kotlin 参数类型不在映射表：{name}::{param_type} —— "
                    "新契约请先更新 KOTLIN_PARAM_TO_DESCRIPTOR"
                )
            parts.append(descriptor)
        return_descriptor = KOTLIN_RETURN_TO_DESCRIPTOR.get(raw_return)
        if return_descriptor is None:
            fail(
                f"Kotlin 返回类型不在映射表：{name}::{raw_return} —— "
                "新契约请先更新 KOTLIN_RETURN_TO_DESCRIPTOR"
            )
        symbol = f"Java_dev_agate_core_Gateway_{name}"
        if symbol in methods:
            fail(f"Kotlin 声明重复：{name}")
        methods[symbol] = f"({''.join(parts)}){return_descriptor}"
    return methods


def parse_rust(source: str) -> dict[str, str]:
    """jni_bridge.rs → {symbol: descriptor}；env/_class 两个隐式参数跳过。"""
    methods: dict[str, str] = {}
    for match in RUST_EXPORT_RE.finditer(source):
        symbol, raw_params, raw_return = match.groups()
        params = [p.strip() for p in raw_params.split(",") if p.strip()]
        parts = []
        for param in params:
            if re.match(r"^(mut\s+)?(env|_class)\s*:", param):
                continue
            _, sep, param_type = param.rpartition(":")
            if not sep:
                fail(f"Rust 参数缺少类型标注：{symbol}({param})")
            param_type = re.sub(r"<[^>]*>", "", param_type).strip()
            descriptor = RUST_PARAM_TO_DESCRIPTOR.get(param_type)
            if descriptor is None:
                fail(
                    f"Rust 参数类型不在映射表：{symbol}::{param_type} —— "
                    "新契约请先更新 RUST_PARAM_TO_DESCRIPTOR"
                )
            parts.append(descriptor)
        return_descriptor = RUST_RETURN_TO_DESCRIPTOR.get(raw_return)
        if return_descriptor is None:
            fail(
                f"Rust 返回类型不在映射表：{symbol}::{raw_return} —— "
                "新契约请先更新 RUST_RETURN_TO_DESCRIPTOR"
            )
        if symbol in methods:
            fail(f"Rust 导出重复：{symbol}")
        methods[symbol] = f"({''.join(parts)}){return_descriptor}"
    return methods


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--kotlin", required=True, type=Path)
    parser.add_argument("--library", required=True, type=Path)
    parser.add_argument("--nm", required=True, type=Path)
    parser.add_argument(
        "--rust",
        type=Path,
        default=None,
        help="jni_bridge.rs 源文件；提供时执行 Kotlin/Rust 描述符互检（R47/T3.5）",
    )
    args = parser.parse_args()

    for path in (args.kotlin, args.library, args.nm):
        if not path.exists():
            parser.error(f"required path does not exist: {path}")

    source = args.kotlin.read_text(encoding="utf-8")
    declared = parse_kotlin(source)
    if not declared:
        print("ERROR: no private external methods found in Gateway.kt", file=sys.stderr)
        return 2

    if args.rust is not None:
        rust_source = args.rust.read_text(encoding="utf-8")
        exported_rust = parse_rust(rust_source)
        missing_in_rust = sorted(set(declared) - set(exported_rust))
        extra_in_rust = sorted(set(exported_rust) - set(declared))
        mismatched = sorted(
            symbol
            for symbol in set(declared) & set(exported_rust)
            if declared[symbol] != exported_rust[symbol]
        )
        if missing_in_rust or extra_in_rust or mismatched:
            for symbol in missing_in_rust:
                print(f"RUST MISSING EXPORT: {symbol}", file=sys.stderr)
            for symbol in extra_in_rust:
                print(f"RUST UNDECLARED EXPORT: {symbol}", file=sys.stderr)
            for symbol in mismatched:
                print(
                    f"SIGNATURE MISMATCH: {symbol}\n"
                    f"  Kotlin: {declared[symbol]}\n"
                    f"  Rust:   {exported_rust[symbol]}",
                    file=sys.stderr,
                )
            return 1
        print(f"Kotlin/Rust signature cross-check: {len(declared)} methods OK")

    result = subprocess.run(
        [str(args.nm), "-D", "--defined-only", str(args.library)],
        check=False,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if result.returncode != 0:
        print(result.stderr, file=sys.stderr)
        return result.returncode or 2

    exported = set()
    for line in result.stdout.splitlines():
        fields = line.split()
        if fields and fields[-1].startswith("Java_dev_agate_core_Gateway_"):
            exported.add(fields[-1])

    missing = sorted(set(declared) - exported)
    extra = sorted(exported - set(declared))
    print(f"Gateway.kt native declarations: {len(declared)}")
    print(f"JNI exports found: {len(exported)}")
    if missing:
        print("MISSING JNI EXPORTS:", *missing, sep="\n  ", file=sys.stderr)
    if extra:
        print("UNDECLARED JNI EXPORTS:", *extra, sep="\n  ", file=sys.stderr)
    if missing or extra:
        return 1
    print("OK: Kotlin declarations and exported JNI symbols match exactly")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
