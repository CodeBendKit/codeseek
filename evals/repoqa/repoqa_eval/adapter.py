"""codeseek CLI 适配层：build 检测 / init / search / 容错 JSON 解析 / 输出组装。

接口事实（已核对源码，勿凭记忆改）：

* 二进制：``rust-core/target/release/codeseek``（``cargo build --release`` 产物）。
* ``codeseek init``：**没有目标目录参数**，必须在含 ``.git`` 的目录内执行，
  否则报 ``No project found. Run codeseek from within a git repository.``。
* ``codeseek search <query> --json --limit <K>``：query 是位置参数。
* 配置文件：``~/.codeseek/config.json``（**不是 toml**），其中 ``embedding`` 段字段为
  ``provider / model / api_token / api_base_url / dimensions / instruct``。
* 检索有两条路径，报告里必须区分：
  - hybrid（``embedding.api_token`` 非空，LanceDB + BM25）：
    JSON 元素字段 ``file_path, symbol_name, code_block, score, symbol_type, language,
    line_start, line_end`` —— **有 ``code_block``**；
  - fallback（无 token）：``graph.find_functions_by_name(query)`` 的**函数名子串匹配**，
    JSON 元素只有 ``name, file_path, line_start, line_end, language`` —— **没有 ``code_block``**。

本模块**绝不使用 shell=True**：所有子进程都以参数列表启动，因此描述里的引号、换行、
``$(...)`` 都不会被 shell 解释，路径含空格也能正常工作。
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any, NamedTuple, Sequence

__all__ = [
    "MODE_FALLBACK",
    "MODE_HYBRID",
    "MODE_UNKNOWN",
    "SearchOutcome",
    "build_output_block",
    "build_outputs",
    "check_embedding_configured",
    "config_path",
    "default_repo_root",
    "detect_mode",
    "ensure_binary",
    "find_binary",
    "init_repo",
    "parse_json_tolerant",
    "search",
    "try_auto_build",
]

MODE_HYBRID = "hybrid"
MODE_FALLBACK = "name_fallback"
MODE_UNKNOWN = "unknown_empty"

#: 仓库内二进制的相对路径。
BINARY_REL_PATH = Path("rust-core") / "target" / "release" / "codeseek"

#: 代码块围栏语言标注（codeseek 的搜索结果里 language 值即这些字符串）。
FENCE_LANGUAGE: dict[str, str] = {
    "python": "python",
    "cpp": "cpp",
    "java": "java",
    "typescript": "typescript",
    "rust": "rust",
    "go": "go",
}

_BUILD_HINT = (
    "未找到 codeseek 二进制。请先构建：\n"
    "  cargo build --release --manifest-path rust-core/Cargo.toml\n"
    "然后用 --codeseek-bin /path/to/codeseek 或环境变量 CODESEEK_BIN 指定路径，"
    "或加 --auto-build 让本脚本自动构建。"
)


def default_repo_root() -> Path:
    """仓库根目录（``<repo>/evals/repoqa/repoqa_eval/adapter.py`` 上溯 3 层）。"""
    return Path(__file__).resolve().parents[3]


def config_path(explicit: str | Path | None = None) -> Path:
    """配置路径：显式参数 > ``CODESEEK_CONFIG_PATH`` > ``~/.codeseek/config.json``。"""
    if explicit:
        return Path(explicit).expanduser()
    env_value = os.environ.get("CODESEEK_CONFIG_PATH", "").strip()
    if env_value:
        return Path(env_value).expanduser()
    return Path.home() / ".codeseek" / "config.json"


def find_binary(
    explicit: str | Path | None = None, repo_root: str | Path | None = None
) -> Path:
    """按优先级定位二进制：显式参数 → ``CODESEEK_BIN`` → 仓库内 release 产物。

    :raises FileNotFoundError: 都找不到时抛出，错误信息包含构建命令。
    """
    candidates: list[Path] = []
    if explicit:
        candidates.append(Path(explicit).expanduser())
    env_value = os.environ.get("CODESEEK_BIN", "").strip()
    if env_value:
        candidates.append(Path(env_value).expanduser())
    root = Path(repo_root).expanduser() if repo_root else default_repo_root()
    candidates.append(root / BINARY_REL_PATH)

    for candidate in candidates:
        if candidate.is_file() and os.access(candidate, os.X_OK):
            return candidate

    searched = "\n".join(f"  - {path}" for path in candidates)
    raise FileNotFoundError(f"{_BUILD_HINT}\n已尝试的路径：\n{searched}")


def try_auto_build(
    repo_root: str | Path | None = None, timeout: float = 3600.0
) -> Path:
    """``--auto-build``：调用 ``cargo build --release``（仅用户在命令行显式要求时执行）。

    构建输出转发到 stderr，避免污染 stdout 上的报告 JSON。
    """
    root = Path(repo_root).expanduser() if repo_root else default_repo_root()
    manifest = root / "rust-core" / "Cargo.toml"
    if not manifest.is_file():
        raise FileNotFoundError(f"找不到 {manifest}，无法自动构建")

    print(f"[adapter] 自动构建：cargo build --release --manifest-path {manifest}", file=sys.stderr)
    completed = subprocess.run(
        [
            "cargo",
            "build",
            "--release",
            "--manifest-path",
            str(manifest),
        ],
        cwd=str(root),
        capture_output=True,
        text=True,
        timeout=timeout,
        check=False,
    )
    if completed.returncode != 0:
        tail = "\n".join((completed.stderr or completed.stdout or "").splitlines()[-40:])
        raise RuntimeError(f"cargo build 失败（退出码 {completed.returncode}）：\n{tail}")

    return find_binary(repo_root=root)


def ensure_binary(
    explicit: str | Path | None = None,
    repo_root: str | Path | None = None,
    auto_build: bool = False,
) -> Path:
    """``find_binary`` + 可选自动构建的组合入口。"""
    try:
        return find_binary(explicit, repo_root)
    except FileNotFoundError:
        if not auto_build:
            raise
        print("[adapter] 未找到二进制，触发 --auto-build ...", file=sys.stderr)
        return try_auto_build(repo_root)


def check_embedding_configured(
    config_path_override: str | Path | None = None,
) -> tuple[bool, str]:
    """判断语义检索（hybrid）是否已配置：``embedding.api_token`` 是否非空。

    读不到/解析失败都返回 ``(False, 原因)``，**不抛异常** —— 评测必须能在没配置的环境下跑完
    （只是分数会退化成名字匹配，报告会明确警示）。

    :return: ``(是否启用语义检索, 人类可读说明)``
    """
    path = config_path(config_path_override)
    if not path.is_file():
        return False, (
            f"config file not found: {path} "
            "(no embedding.api_token; search degrades to function-name substring matching)"
        )

    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        return False, f"cannot parse config file: {path} ({type(exc).__name__}: {exc})"

    embedding: Any = None
    if isinstance(payload, dict):
        embedding = payload.get("embedding")
        if embedding is None and isinstance(payload.get("codebase"), dict):
            # 兼容旧版 config.toml 转 json 后的嵌套形态。
            embedding = payload["codebase"].get("embedding")

    if not isinstance(embedding, dict):
        return False, f"{path} has no embedding section (no embedding.api_token)"

    token = embedding.get("api_token")
    if not isinstance(token, str) or not token.strip():
        return False, (
            f"{path} has an empty embedding.api_token "
            "(search degrades to function-name substring matching)"
        )

    model = embedding.get("model") or "<model not set>"
    return True, f"embedding configured (model={model}, api_token length {len(token.strip())})"


def init_repo(
    binary: Path, repo_dir: Path, timeout: float = 1800.0
) -> tuple[bool, str]:
    """在 ``repo_dir`` 下执行 ``codeseek init``。

    进度条（indicatif）会往 stdout/stderr 写控制字符，**不要解析输出内容**，
    只判断退出码并保留最后若干行用于报错。
    """
    try:
        completed = subprocess.run(
            [str(binary), "init"],
            cwd=str(repo_dir),
            capture_output=True,
            text=True,
            timeout=timeout,
            check=False,
        )
    except subprocess.TimeoutExpired:
        return False, f"codeseek init 超时（>{timeout:.0f}s）"
    except (OSError, ValueError) as exc:
        return False, f"无法执行 {binary}：{type(exc).__name__}: {exc}"

    output = "\n".join(
        line for line in ((completed.stdout or "") + (completed.stderr or "")).splitlines() if line.strip()
    )
    tail = "\n".join(output.splitlines()[-12:])
    return completed.returncode == 0, tail


class SearchOutcome(NamedTuple):
    """一次 ``codeseek search`` 的结果。

    字段顺序即返回顺序，便于 ``results, mode, error = outcome`` 解构；
    ``mode`` 取值见 ``MODE_HYBRID`` / ``MODE_FALLBACK`` / ``MODE_UNKNOWN``。
    """

    results: list[dict[str, Any]]
    mode: str
    error: str
    elapsed_s: float = 0.0


def parse_json_tolerant(stdout: str) -> Any | None:
    """从可能被日志污染的 stdout 中解析 JSON。

    codeseek 的 tracing/indicatif 输出有可能混进 stdout（已实测存在这种风险），
    因此按下列顺序容错：

    1. 整段 ``json.loads``；
    2. 定位第一个 ``[`` 与最后一个 ``]``（或 ``{`` / ``}``）后截取再解析；
    3. 逐行查找以 ``{`` / ``[`` 开头且能单独解析的行（NDJSON 场景）；
    4. 全部失败返回 ``None``。
    """
    if not stdout:
        return None

    text = stdout.strip()
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        pass

    for opener, closer in (("[", "]"), ("{", "}")):
        start = text.find(opener)
        end = text.rfind(closer)
        if start != -1 and end > start:
            try:
                return json.loads(text[start : end + 1])
            except json.JSONDecodeError:
                continue

    # NDJSON：逐个候选行尝试，优先返回第一个解析成功的 list，其次 dict。
    fallback: Any | None = None
    for line in text.splitlines():
        stripped = line.strip()
        if not stripped or stripped[0] not in "[{":
            continue
        try:
            parsed = json.loads(stripped)
        except json.JSONDecodeError:
            continue
        if isinstance(parsed, list):
            return parsed
        if fallback is None and isinstance(parsed, dict):
            fallback = parsed
    return fallback


def _preview(text: str, limit: int = 500) -> str:
    """截断诊断信息，避免把整段日志写进 raw 结果。"""
    cleaned = re.sub(r"\x1b\[[0-9;]*m", "", text or "")
    return cleaned[:limit]


def search(
    binary: Path,
    repo_dir: Path,
    query: str,
    limit: int = 5,
    timeout: float = 180.0,
) -> SearchOutcome:
    """执行 ``codeseek search <query> --json --limit <K>`` 并返回结构化结果。

    query 通过参数列表传入（**不用 shell=True**），因此描述中的引号/换行/反引号
    都不会被杀掉或触发注入。
    """
    argv = [str(binary), "search", query, "--json", "--limit", str(int(limit))]
    started = time.monotonic()
    try:
        completed = subprocess.run(
            argv,
            cwd=str(repo_dir),
            capture_output=True,
            text=True,
            timeout=timeout,
            check=False,
        )
    except subprocess.TimeoutExpired:
        return SearchOutcome([], MODE_UNKNOWN, f"search 超时（>{timeout:.0f}s）", time.monotonic() - started)
    except (OSError, ValueError) as exc:
        return SearchOutcome(
            [], MODE_UNKNOWN, f"无法执行 {binary}：{type(exc).__name__}: {exc}", time.monotonic() - started
        )

    elapsed = time.monotonic() - started
    parsed = parse_json_tolerant(completed.stdout)

    if parsed is None:
        detail = _preview(completed.stdout) or _preview(completed.stderr)
        return SearchOutcome(
            [],
            MODE_UNKNOWN,
            f"stdout 不是可解析 JSON（退出码 {completed.returncode}）：{detail}",
            elapsed,
        )

    if isinstance(parsed, dict):
        # 防御：万一某版本把结果包在 {"data": [...]} 里。
        for key in ("data", "results", "items"):
            if isinstance(parsed.get(key), list):
                parsed = parsed[key]
                break
        else:
            return SearchOutcome(
                [], MODE_UNKNOWN, f"JSON 是对象而非数组：{_preview(json.dumps(parsed, ensure_ascii=False))}", elapsed
            )

    results = [item for item in parsed if isinstance(item, dict)]
    mode = detect_mode(results)
    error = ""
    if completed.returncode != 0:
        error = f"退出码 {completed.returncode}：{_preview(completed.stderr) or _preview(completed.stdout)}"
    return SearchOutcome(results, mode, error, elapsed)


def detect_mode(results: Sequence[dict[str, Any]]) -> str:
    """判定本次检索走的是哪条路径（以 JSON 结构为判据）。

    * 任一元素含 ``code_block`` 键 → ``hybrid``（真语义检索）；
    * 否则若结果非空 → ``name_fallback``（函数名子串匹配）；
    * 空结果 → ``unknown_empty``（无法从结构判断，报告里会结合 config 再解释）。
    """
    if not results:
        return MODE_UNKNOWN
    if any("code_block" in item for item in results):
        return MODE_HYBRID
    return MODE_FALLBACK


def _resolve_repo_file(repo_dir: Path, file_path: str) -> Path | None:
    """把检索结果里的 ``file_path`` 解析为仓库内的真实路径（越界/不存在返回 None）。"""
    if not file_path:
        return None
    candidate = Path(file_path)
    if not candidate.is_absolute():
        candidate = repo_dir / candidate
    try:
        resolved = candidate.resolve()
        root = repo_dir.resolve()
    except OSError:
        return None
    if not (resolved == root or root in resolved.parents):
        return None
    return resolved if resolved.is_file() else None


def _slice_lines(path: Path, start: int, end: int) -> str:
    """按行号切片；codeseek 的 ``line_start`` 从 1 开始（闭区间）。

    Rust 端 ``line_start/line_end`` 来自 tree-sitter 的行号（1-based），因此按
    ``lines[start-1:end]`` 取；若结果为空（例如某些 fallback 路径给出 0-based），
    再尝试 ``lines[start:end+1]``，两种都不行则返回空串。
    """
    try:
        text = path.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return ""

    lines = text.split("\n")
    start = max(int(start), 0)
    end = max(int(end), 0)

    primary = "\n".join(lines[max(start - 1, 0) : end]) if end >= start else ""
    if primary.strip():
        return primary
    secondary = "\n".join(lines[start : end + 1])
    return secondary if secondary.strip() else ""


def build_item_code(item: dict[str, Any], repo_dir: Path) -> str:
    """从单条检索结果取函数源码：优先 ``code_block``，否则按行号切片。"""
    code_block = item.get("code_block")
    if isinstance(code_block, str) and code_block.strip():
        return code_block

    file_path = item.get("file_path") or ""
    line_start = item.get("line_start")
    line_end = item.get("line_end")
    if not file_path or line_start is None or line_end is None:
        return ""

    resolved = _resolve_repo_file(repo_dir, str(file_path))
    if resolved is None:
        return ""
    return _slice_lines(resolved, int(line_start), int(line_end))


def build_output_block(item: dict[str, Any], repo_dir: Path, language: str) -> str:
    """把一条检索结果组装成 fenced 代码块（官方 sanitize 能识别的形态）。

    空结果 / 取不到源码时返回空串 —— 由 scorer 记为 fail，不抛异常。
    """
    code = build_item_code(item, repo_dir)
    if not code.strip():
        return ""
    fence = FENCE_LANGUAGE.get(language, "text")
    return f"```{fence}\n{code.rstrip()}\n```"


def build_outputs(
    results: Sequence[dict[str, Any]], repo_dir: Path, language: str
) -> list[str]:
    """按检索顺序组装输出列表，**跳过取不到源码的项**。

    官方只取 ``output[0]``；因此"Top-1 结果取不到源码"时，与其直接判 0，
    不如把下一条能取到源码的结果顶上（并在 raw 里保留 ``results_count`` 供追溯）。
    两个行为都记录在 README 的"与官方口径的差异"一节。
    """
    blocks: list[str] = []
    for item in results:
        block = build_output_block(item, repo_dir, language)
        if block:
            blocks.append(block)
    return blocks


@dataclass
class BinaryInfo:
    """二进制元信息（写进 report.meta，便于定位"这次跑的是哪个 build"）。"""

    path: str
    exists: bool = True
    size_bytes: int = 0
    mtime_iso: str = ""
    sha256_prefix: str = ""


def describe_binary(binary: Path) -> dict[str, Any]:
    """采集二进制的路径 / 大小 / mtime / 前缀指纹。"""
    import hashlib
    from datetime import datetime, timezone

    info = BinaryInfo(path=str(binary))
    try:
        stat = binary.stat()
        info.size_bytes = stat.st_size
        info.mtime_iso = datetime.fromtimestamp(stat.st_mtime, tz=timezone.utc).isoformat()
        digest = hashlib.sha256()
        with open(binary, "rb") as handle:
            for chunk in iter(lambda: handle.read(1024 * 1024), b""):
                digest.update(chunk)
        info.sha256_prefix = digest.hexdigest()[:16]
    except OSError:
        info.exists = False
    return {
        "path": info.path,
        "exists": info.exists,
        "size_bytes": info.size_bytes,
        "mtime_iso": info.mtime_iso,
        "sha256_prefix": info.sha256_prefix,
    }
