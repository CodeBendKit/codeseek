"""codeseek CLI 适配层：二进制定位 / init / search / 容错 JSON 解析 / 结果 → doc_id 反查。

接口事实（与 ``evals/repoqa/repoqa_eval/adapter.py`` 逐条核对过，勿凭记忆改）：

* 二进制：``rust-core/target/release/codeseek``（``cargo build --release`` 产物）。
* ``codeseek init``：**没有目标目录参数**，必须在含 ``.git`` 的目录内执行，
  否则报 ``No project found. Run codeseek from within a git repository.``。
* ``codeseek search --json --limit <K> -- <query>``：query 是位置参数，必须放在 ``--`` 之后
  （以 ``-`` 开头的 query 否则会被 clap 当成未知 flag），``cwd`` 必须是语料目录。
* 配置文件：``~/.codeseek/config.json``（**不是 toml**），``embedding`` 段字段为
  ``provider / model / api_token / api_base_url / dimensions / instruct``。
* 检索有两条路径，报告里必须区分：
  - hybrid（``embedding.api_token`` 非空，LanceDB + BM25）：
    JSON 元素字段 ``file_path, symbol_name, code_block, score, symbol_type, language,
    line_start, line_end`` —— **有 ``code_block``**；
  - fallback（无 token）：``graph.find_functions_by_name(query)`` 的**函数名子串匹配**，
    JSON 元素只有 ``name, file_path, line_start, line_end, language`` —— **没有 ``code_block``**。

本模块**绝不使用 shell=True**：所有子进程都以参数列表启动，因此 query 文本里的引号、
换行、``$(...)`` 都不会被 shell 解释，路径含空格也能正常工作。

与 repoqa 的差异只有一处：MTEB(Code) 关心「文档 id 的排名」，不关心代码块内容，因此
新增 :func:`resolve_doc_ids` 把每条检索结果映射回 ``docmap.json`` 里的 doc_id。
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import Any, Iterable, NamedTuple, Sequence

__all__ = [
    "MODE_FALLBACK",
    "MODE_HYBRID",
    "MODE_UNKNOWN",
    "DocResolution",
    "SearchOutcome",
    "check_embedding_configured",
    "config_path",
    "default_repo_root",
    "describe_binary",
    "detect_mode",
    "ensure_binary",
    "find_binary",
    "init_repo",
    "parse_json_tolerant",
    "resolve_doc_ids",
    "search",
    "try_auto_build",
]

MODE_HYBRID = "hybrid"
MODE_FALLBACK = "name_fallback"
MODE_UNKNOWN = "unknown_empty"

#: 仓库内二进制的相对路径。
BINARY_REL_PATH = Path("rust-core") / "target" / "release" / "codeseek"

#: 检索结果里可能承载文件路径的 key（不同版本/不同路径下字段名略有差异）。
PATH_KEYS: tuple[str, ...] = ("file_path", "path", "file", "relative_path", "filepath")

#: 兜底：直接命中 doc_id 的 symbol 字段。
SYMBOL_KEYS: tuple[str, ...] = ("name", "symbol_name")

_BUILD_HINT = (
    "未找到 codeseek 二进制。请先构建：\n"
    "  cargo build --release --manifest-path rust-core/Cargo.toml\n"
    "然后用 --codeseek-bin /path/to/codeseek 或环境变量 CODESEEK_BIN 指定路径，\n"
    "或加 --auto-build 让本脚本自动构建。"
)


def default_repo_root() -> Path:
    """仓库根目录（``<repo>/evals/mtebcode/mtebcode_eval/adapter.py`` 上溯 3 层）。"""
    return Path(__file__).resolve().parents[3]


def config_path(explicit: str | Path | None = None) -> Path:
    """配置路径：显式参数 > ``CODESEEK_CONFIG_PATH`` > ``~/.codeseek/config.json``。"""
    if explicit:
        return Path(explicit).expanduser()
    env_value = os.environ.get("CODESEEK_CONFIG_PATH", "").strip()
    if env_value:
        return Path(env_value).expanduser()
    return Path.home() / ".codeseek" / "config.json"


def find_binary(explicit: str | Path | None = None, repo_root: str | Path | None = None) -> Path:
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


def try_auto_build(repo_root: str | Path | None = None, timeout: float = 3600.0) -> Path:
    """``--auto-build``：调用 ``cargo build --release``（只在命令行显式要求时执行）。"""
    root = Path(repo_root).expanduser() if repo_root else default_repo_root()
    manifest = root / "rust-core" / "Cargo.toml"
    if not manifest.is_file():
        raise FileNotFoundError(f"找不到 {manifest}，无法自动构建")

    print(f"[adapter] 自动构建：cargo build --release --manifest-path {manifest}", file=sys.stderr)
    completed = subprocess.run(
        ["cargo", "build", "--release", "--manifest-path", str(manifest)],
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


def check_embedding_configured(config_path_override: str | Path | None = None) -> tuple[bool, str]:
    """判断语义检索（hybrid）是否已配置：``embedding.api_token`` 是否非空。

    读不到 / 解析失败都返回 ``(False, 原因)``，**不抛异常** —— 评测必须能在没配置的
    环境下跑完（只是分数会退化成名字匹配，报告会明确警示）。
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


def init_repo(binary: Path, repo_dir: Path, timeout: float = 1800.0) -> tuple[bool, str]:
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
    """从可能被日志污染的 stdout 中解析 JSON（**直接复用 repoqa 的实现**）。

    按下列顺序容错：

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
    limit: int = 20,
    timeout: float = 180.0,
) -> SearchOutcome:
    """执行 ``codeseek search --json --limit <K> -- <query>`` 并返回结构化结果。

    query 通过参数列表传入（**不用 shell=True**），因此 query 文本里的引号/换行/反引号
    都不会被杀掉或触发注入。

    **选项必须排在 query 之前，且 query 必须放在 ``--`` 之后**：``query`` 是 clap 的
    位置参数，若它落在选项区且以 ``-`` 开头（CodeSearchNet 里存在整行 ``-----`` 这类
    查询），clap 会把它当成未知 flag 并以退出码 2 失败（实测报
    ``error: unexpected argument '-----------------------------------' found``）。
    ``--`` 是 clap 的转义分隔符，之后的所有值无条件按位置参数解释，因此无论 query
    以多少个 ``-`` 开头都不会被误判。rust 侧同一子命令还额外设了
    ``allow_hyphen_values``，两条防线互为备份（见 ``rust-core/src/cli/args.rs``）。
    """
    argv = [str(binary), "search", "--json", "--limit", str(int(limit)), "--", query]
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
                [],
                MODE_UNKNOWN,
                f"JSON 是对象而非数组：{_preview(json.dumps(parsed, ensure_ascii=False))}",
                elapsed,
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


# ---------------------------------------------------------------------------
# 检索结果 → doc_id 反查
# ---------------------------------------------------------------------------


class DocResolution(NamedTuple):
    """一次检索结果映射回 doc_id 的产物。"""

    ranked: list[str]
    n_returned: int
    n_mapped: int
    n_unmapped: int

    @property
    def top(self) -> str | None:
        return self.ranked[0] if self.ranked else None


def _normalise_path(value: Any) -> str:
    """统一成全斜杠、无 ``./`` 前缀的字符串。"""
    text = str(value or "").replace("\\", "/").strip()
    while text.startswith("./"):
        text = text[2:]
    return text


def _index_basenames(docmap: dict[str, str]) -> dict[str, list[str]]:
    """``basename → [docmap 的 key...]``，供「basename 兜底」使用。"""
    index: dict[str, list[str]] = {}
    for relative in docmap:
        name = PurePosixPath(relative).name
        index.setdefault(name, []).append(relative)
    return index


def _first_existing(candidates: Iterable[str], docmap: dict[str, str]) -> str | None:
    for candidate in candidates:
        if candidate and candidate in docmap:
            return docmap[candidate]
    return None


def _resolve_one(
    item: dict[str, Any],
    docmap: dict[str, str],
    basename_index: dict[str, list[str]],
    doc_ids: set[str],
    corpus_root: str | None,
) -> str | None:
    """把单条检索结果映射成 doc_id（四级规则，逐级降级）。"""
    raw_path = ""
    for key in PATH_KEYS:
        value = item.get(key)
        if isinstance(value, str) and value.strip():
            raw_path = value
            break

    text = _normalise_path(raw_path)
    if text:
        # 1a) 直接查 docmap（codeseek 从 cwd 出发返回相对路径时命中）
        hit = _first_existing([text], docmap)
        if hit is not None:
            return hit

        # 1b) 去掉 corpus_dir 前缀（绝对路径场景）
        if corpus_root:
            root = corpus_root.rstrip("/")
            if text == root or text.startswith(root + "/"):
                stripped = text[len(root) + 1 :] if text != root else ""
                hit = _first_existing([stripped], docmap)
                if hit is not None:
                    return hit
        # 1c) 绝对路径但 corpus_dir 未知/不同前缀：尝试把「任意公共后缀」匹配到某个 key
        absolute_hit = _match_by_suffix(text, docmap)
        if absolute_hit is not None:
            return absolute_hit

        # 2) basename 反向匹配（文件名里有 doc_id 的短哈希，正常情况下唯一）
        name = PurePosixPath(text).name
        candidates = basename_index.get(name, [])
        if len(candidates) == 1:
            return docmap[candidates[0]]
        if len(candidates) > 1:
            # 多个同名：优先选与 text 后缀最长的那个（确定性）
            ranked = sorted(candidates, key=lambda key: (-len(_common_suffix(key, text)), key))
            return docmap[ranked[0]]

    # 3) 兜底：符号名恰好等于某个 doc_id
    for key in SYMBOL_KEYS:
        value = item.get(key)
        if isinstance(value, str) and value.strip() and value.strip() in doc_ids:
            return value.strip()

    # 4) 失败
    return None


def _match_by_suffix(text: str, docmap: dict[str, str]) -> str | None:
    """``text`` 以某个 docmap key 结尾时命中（唯一才返回）。"""
    if not text:
        return None
    matches = [key for key in docmap if text.endswith("/" + key) or text == key]
    if len(matches) == 1:
        return docmap[matches[0]]
    if len(matches) > 1:
        best = max(matches, key=len)
        return docmap[best]
    return None


def _common_suffix(left: str, right: str) -> str:
    """两个路径的公共后缀（按 ``/`` 分段比较）。"""
    left_parts = left.split("/")[::-1]
    right_parts = right.split("/")[::-1]
    common: list[str] = []
    for a, b in zip(left_parts, right_parts):
        if a != b:
            break
        common.append(a)
    return "/".join(reversed(common))


def resolve_doc_ids(
    results: Sequence[dict[str, Any]],
    docmap: dict[str, str],
    corpus_dir: str | Path | None = None,
) -> DocResolution:
    """把检索结果映射回 doc_id，并**去重保序**。

    规则（逐级降级，全部失败记 ``unmapped``）：

    1. ``file_path`` / ``path`` 规范成相对 ``corpus_dir`` 的 POSIX 路径后查 ``docmap``
       （同时处理绝对路径、``./`` 前缀、含 ``corpus_dir`` 前缀的路径）；
    2. 查不到则用 ``os.path.basename`` 反向匹配 ``docmap`` 的所有 key；
    3. 仍查不到则看元素里的 ``name`` / ``symbol_name`` 是否恰好等于某个 doc_id（兜底）；
    4. 都失败记 ``None``。

    去重保序：同一个 doc_id 只保留**首次出现**的排名 —— codeseek 可能对同一个文件
    返回多个 chunk，若不去重，nDCG 会被「同一文档占满 Top-K」严重高估。
    """
    corpus_root = str(Path(corpus_dir).resolve()) if corpus_dir else None
    basename_index = _index_basenames(docmap)
    doc_ids = set(docmap.values())

    ranked: list[str] = []
    seen: set[str] = set()
    n_mapped = 0
    n_unmapped = 0
    n_returned = 0

    for item in results:
        if not isinstance(item, dict):
            continue
        n_returned += 1
        doc_id = _resolve_one(item, docmap, basename_index, doc_ids, corpus_root)
        if doc_id is None:
            n_unmapped += 1
            continue
        n_mapped += 1
        if doc_id in seen:
            continue
        seen.add(doc_id)
        ranked.append(doc_id)

    return DocResolution(ranked=ranked, n_returned=n_returned, n_mapped=n_mapped, n_unmapped=n_unmapped)


@dataclass
class BinaryInfo:
    """二进制元信息（写进 report.meta，便于定位「这次跑的是哪个 build」）。"""

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
