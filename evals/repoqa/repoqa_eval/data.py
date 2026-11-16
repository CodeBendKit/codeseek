"""RepoQA 数据集：下载 / 缓存 / 子集抽样 / 仓库快照落盘 / Task 定义。

数据形态（已实测确认，切勿凭印象修改）：

* 官方发布物是**单个 gzip JSON**（12.5 MB gz → 71.5 MB JSON）。
* 顶层是 ``dict``，key 为 6 种语言：``python, cpp, java, typescript, rust, go``。
* 每种语言是长度 10 的 ``list``，元素为 repo 条目，字段逐字为：
  ``repo, commit_sha, entrypoint_path, topic, content, dependency, functions, needles``。
* ``repo["content"]`` 是 ``dict[相对路径] -> 该文件完整文本``（**已含全部源文件，不必 clone**）。
* ``repo["needles"]`` 长度恰为 10，字段逐字为：
  ``name, start_line, end_line, start_byte, end_byte, global_start_line, global_end_line,
  global_start_byte, global_end_byte, code_ratio, path, description``。
  ``start_line`` / ``end_line`` 是**文件内**行号，官方切片方式：
  ``"\\n".join(content[path].split("\\n")[start_line:end_line])``。
* ``repo["functions"]`` 仅用于建索引，**判分不用**。
"""

from __future__ import annotations

import gzip
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
import urllib.error
import urllib.request
from dataclasses import asdict, dataclass
from pathlib import Path, PurePosixPath
from typing import Any, Iterable, Sequence

__all__ = [
    "ALL_LANGUAGES",
    "DEFAULT_LANGUAGES",
    "DATASET_RELEASE",
    "DATASET_URL",
    "NEEDLES_PER_REPO",
    "Task",
    "dataset_fingerprint",
    "download_dataset",
    "load_dataset",
    "load_tasks",
    "materialize_repo",
    "needle_text",
    "read_workdir_meta",
    "repo_dir",
    "repo_slug",
    "select_subset",
    "validate_dataset",
    "write_tasks",
    "write_workdir_meta",
]

#: 官方 release（2024-06-23）。
DATASET_RELEASE = "2024-06-23"
DATASET_URL = (
    "https://github.com/evalplus/repoqa_release/releases/download/"
    f"{DATASET_RELEASE}/repoqa-{DATASET_RELEASE}.json.gz"
)
DATASET_GZ_NAME = f"repoqa-{DATASET_RELEASE}.json.gz"
DATASET_JSON_NAME = f"repoqa-{DATASET_RELEASE}.json"

#: 发布物实际包含的语言（README 写 5 语言 500 题，实测是 6 语言 600 题）。
ALL_LANGUAGES: tuple[str, ...] = ("python", "cpp", "java", "typescript", "rust", "go")
#: 默认评测口径：5 语言，不含 go（与官方 README 的 500 题口径对齐）。
DEFAULT_LANGUAGES: tuple[str, ...] = ("python", "cpp", "java", "typescript", "rust")

#: 每个 repo 的 needle 数量（官方固定 10，position_ratio 的 /10 由此而来）。
NEEDLES_PER_REPO = 10

#: 下载 User-Agent：GitHub release 对无 UA 的请求有时直接 403。
USER_AGENT = "codeseek-repoqa-eval/1.0 (+https://github.com/evalplus/repoqa_release)"

#: workdir 内的固定文件名。
TASKS_FILE = "tasks.jsonl"
RAW_DIR = "raw"
REPOS_DIR = "repos"
WORKDIR_META = "meta.json"
REPORT_JSON = "report.json"
REPORT_MD = "report.md"

#: 写入仓库快照的清单文件名（用于 prepare 幂等跳过的判据）。
MANIFEST_NAME = ".repoqa_manifest.json"


# ---------------------------------------------------------------------------
# Task
# ---------------------------------------------------------------------------


@dataclass
class Task:
    """一道 RepoQA 题（= 某 repo 的某一条 needle）。

    ``needle_token_start`` / ``needle_token_end`` 在官方数据里没有"token 级"字段，
    这里用 ``needle.start_byte`` / ``needle.end_byte``（文件内字节偏移）填充，
    仅用于报告展示，**不参与判定**；缺失时置 0。
    """

    id: str
    language: str
    repo: str
    needle_name: str
    needle_path: str
    start_line: int
    end_line: int
    description: str
    position_ratio: float
    needle_token_start: int = 0
    needle_token_end: int = 0
    # 下面是定位 dataset 内部条目所必需的索引信息（不进报告口径，只做查表）。
    repo_index: int = 0
    needle_index: int = 0
    repo_slug: str = ""
    entrypoint_path: str = ""
    commit_sha: str = ""

    def to_dict(self) -> dict[str, Any]:
        return asdict(self)

    @classmethod
    def from_dict(cls, payload: dict[str, Any]) -> "Task":
        known = {f for f in cls.__dataclass_fields__}  # type: ignore[attr-defined]
        return cls(**{k: v for k, v in payload.items() if k in known})


def repo_slug(repo: str) -> str:
    """``psf/black`` → ``psf__black``（落盘目录名，替换 ``/`` 以保持单层目录）。"""
    return repo.replace("/", "__")


def repo_dir(workdir: Path | str, language: str, repo: str) -> Path:
    """仓库快照目录：``<workdir>/repos/<language>/<owner>__<name>``。"""
    return Path(workdir) / REPOS_DIR / language / repo_slug(repo)


def task_id(language: str, repo: str, needle_index: int) -> str:
    """每题稳定 id（同时作为 ``raw/<task_id>.json`` 的文件名，必须文件系统安全）。"""
    return f"{language}__{repo_slug(repo)}__n{needle_index:02d}"


# ---------------------------------------------------------------------------
# needle 切片
# ---------------------------------------------------------------------------


def needle_text(content_text: str, start_line: int, end_line: int) -> str:
    """官方 needle 切片：``"\\n".join(content[path].split("\\n")[start_line:end_line])``。

    行号语义是**文件内、0-based 起点、end 不含**（官方 ``compute_score.py`` 即如此），
    不要改成 1-based。
    """
    return "\n".join(content_text.split("\n")[start_line:end_line])


# ---------------------------------------------------------------------------
# 下载 / 加载
# ---------------------------------------------------------------------------


def _log(message: str) -> None:
    print(message, file=sys.stderr, flush=True)


def _proxy_opener(proxy: str | None) -> urllib.request.OpenerDirector:
    """构造 opener；``proxy`` 为空时沿用环境变量（``https_proxy`` 等）。"""
    if proxy:
        handler = urllib.request.ProxyHandler({"http": proxy, "https": proxy})
        return urllib.request.build_opener(handler)
    return urllib.request.build_opener()


def download_dataset(
    cache_dir: Path | str, proxy: str | None = None, timeout: float = 120.0
) -> Path:
    """把 gz 下载并解压成 json，返回 json 路径（已存在则直接复用）。

    ``REPOQA_DATA_OVERRIDE_PATH`` 优先级最高：指向本地已有 json（或 gz）时不再联网，
    便于离线环境与重复评测。

    :raises RuntimeError: 下载/解压失败，错误信息包含代理提示与可操作建议。
    """
    override = os.environ.get("REPOQA_DATA_OVERRIDE_PATH", "").strip()
    if override:
        path = Path(override).expanduser()
        if not path.is_file():
            raise RuntimeError(
                f"REPOQA_DATA_OVERRIDE_PATH 指向的文件不存在：{path}"
            )
        if path.suffix == ".gz":
            _log(f"[data] 使用 REPOQA_DATA_OVERRIDE_PATH（gz）: {path}")
            return _gunzip_to_cache(path, Path(cache_dir))
        _log(f"[data] 使用 REPOQA_DATA_OVERRIDE_PATH: {path}")
        return path

    cache = Path(cache_dir).expanduser()
    cache.mkdir(parents=True, exist_ok=True)
    json_path = cache / DATASET_JSON_NAME

    if json_path.is_file() and json_path.stat().st_size > 0:
        _log(f"[data] 复用已解压数据集: {json_path} ({_human_size(json_path.stat().st_size)})")
        return json_path

    gz_path = cache / DATASET_GZ_NAME
    if not (gz_path.is_file() and gz_path.stat().st_size > 0):
        _log(f"[data] 下载 {DATASET_URL}")
        _download_file(DATASET_URL, gz_path, proxy=proxy, timeout=timeout)
    else:
        _log(f"[data] 复用已下载 gz: {gz_path} ({_human_size(gz_path.stat().st_size)})")

    return _gunzip_to_cache(gz_path, cache)


def _human_size(size: int) -> str:
    value = float(size)
    for unit in ("B", "KB", "MB", "GB"):
        if value < 1024 or unit == "GB":
            return f"{value:.1f}{unit}"
        value /= 1024
    return f"{value:.1f}GB"


def _download_file(url: str, dest: Path, proxy: str | None, timeout: float) -> None:
    """流式下载到 ``.part`` 再原子改名；失败时抛出可操作的异常。"""
    part = dest.with_suffix(dest.suffix + ".part")
    request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    opener = _proxy_opener(proxy)
    try:
        with opener.open(request, timeout=timeout) as response, open(part, "wb") as handle:
            total = response.headers.get("Content-Length")
            _log(f"[data] HTTP {getattr(response, 'status', '?')}，Content-Length={total or '未知'}")
            shutil.copyfileobj(response, handle, length=1024 * 256)
    except urllib.error.HTTPError as exc:
        part.unlink(missing_ok=True)
        raise RuntimeError(
            f"下载失败：HTTP {exc.code} {exc.reason} —— {url}\n"
            "可尝试：\n"
            "  1) 设置代理后重试：--proxy http://127.0.0.1:7897"
            "（或 export https_proxy=... http_proxy=...）\n"
            "  2) 手动下载 json.gz 后设置 REPOQA_DATA_OVERRIDE_PATH=/path/to/file.json.gz"
        ) from exc
    except (urllib.error.URLError, TimeoutError, OSError) as exc:
        part.unlink(missing_ok=True)
        raise RuntimeError(
            f"下载失败：{type(exc).__name__}: {exc} —— {url}\n"
            "可尝试：\n"
            "  1) 设置代理后重试：--proxy http://127.0.0.1:7897"
            "（或 export https_proxy=... http_proxy=...）\n"
            "  2) 手动下载 json.gz 后设置 REPOQA_DATA_OVERRIDE_PATH=/path/to/file.json.gz"
        ) from exc

    part.replace(dest)


def _gunzip_to_cache(gz_path: Path, cache_dir: Path) -> Path:
    """把 gz 解压成 ``cache_dir/repoqa-2024-06-23.json``（复用已解压结果）。"""
    cache_dir.mkdir(parents=True, exist_ok=True)
    json_path = cache_dir / DATASET_JSON_NAME
    if json_path.is_file() and json_path.stat().st_size > 0:
        _log(f"[data] 复用已解压数据集: {json_path}")
        return json_path

    _log(f"[data] 解压 {gz_path.name} → {json_path.name} ...")
    tmp_fd, tmp_name = tempfile.mkstemp(dir=str(cache_dir), suffix=".json.tmp")
    os.close(tmp_fd)
    tmp_path = Path(tmp_name)
    try:
        with gzip.open(gz_path, "rb") as src, open(tmp_path, "wb") as dst:
            shutil.copyfileobj(src, dst, length=1024 * 1024)
        tmp_path.replace(json_path)
    except OSError as exc:
        tmp_path.unlink(missing_ok=True)
        raise RuntimeError(f"解压失败：{gz_path} —— {exc}") from exc

    _log(f"[data] 解压完成: {json_path} ({_human_size(json_path.stat().st_size)})")
    return json_path


def load_dataset(path: Path | str) -> dict[str, list[dict[str, Any]]]:
    """读取 RepoQA json，并做最小结构校验。"""
    path = Path(path)
    if not path.is_file():
        raise FileNotFoundError(f"数据集不存在：{path}（先运行 `prepare` 或设置 REPOQA_DATA_OVERRIDE_PATH）")

    if path.suffix == ".gz":
        with gzip.open(path, "rt", encoding="utf-8") as handle:
            dataset = json.load(handle)
    else:
        with open(path, "r", encoding="utf-8") as handle:
            dataset = json.load(handle)

    validate_dataset(dataset)
    return dataset


def validate_dataset(dataset: Any) -> None:
    """校验顶层结构：6 语言、每语言 10 个 repo、每 repo 10 条 needle。"""
    if not isinstance(dataset, dict):
        raise ValueError(f"数据集顶层应为 dict，实际为 {type(dataset).__name__}")

    missing = [lang for lang in ALL_LANGUAGES if lang not in dataset]
    if missing:
        raise ValueError(f"数据集缺少语言 key：{missing}（实际 keys={sorted(dataset)[:10]}）")

    for lang in ALL_LANGUAGES:
        repos = dataset[lang]
        if not isinstance(repos, list) or not repos:
            raise ValueError(f"{lang}: 应为非空 list，实际为 {type(repos).__name__}")
        for index, repo in enumerate(repos):
            for key in ("repo", "content", "needles"):
                if key not in repo:
                    raise ValueError(f"{lang}[{index}] 缺少字段 {key}")
            if not isinstance(repo["content"], dict) or not isinstance(repo["needles"], list):
                raise ValueError(f"{lang}[{index}] content/needles 类型异常")


def dataset_fingerprint(path: Path | str) -> dict[str, Any]:
    """数据集指纹（写进 report.meta，便于复现）：路径/大小/前 16 位 sha256。"""
    path = Path(path)
    if not path.is_file():
        return {"path": str(path), "exists": False}
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return {
        "path": str(path),
        "exists": True,
        "size_bytes": path.stat().st_size,
        "sha256_prefix": digest.hexdigest()[:16],
        "release": DATASET_RELEASE,
        "url": DATASET_URL,
    }


# ---------------------------------------------------------------------------
# 子集抽样
# ---------------------------------------------------------------------------


def select_subset(
    dataset: dict[str, list[dict[str, Any]]],
    languages: Sequence[str],
    repos_per_language: int = 2,
) -> list[Task]:
    """按语言各取**前 N 个 repo**，每个 repo 取全部 10 条 needle（完全确定性，不随机）。

    默认 5 语言 × 2 repo × 10 needle = 100 题。
    """
    unknown = [lang for lang in languages if lang not in dataset]
    if unknown:
        raise ValueError(f"未知语言：{unknown}；可用：{sorted(dataset)}")

    tasks: list[Task] = []
    for language in languages:
        repos = dataset[language][:repos_per_language]
        for repo_index, repo in enumerate(repos):
            needles = repo["needles"]
            for needle_index, needle in enumerate(needles):
                tasks.append(
                    Task(
                        id=task_id(language, repo["repo"], needle_index),
                        language=language,
                        repo=repo["repo"],
                        needle_name=needle["name"],
                        needle_path=needle["path"],
                        start_line=int(needle["start_line"]),
                        end_line=int(needle["end_line"]),
                        description=needle["description"],
                        # 官方口径：假定每条 needle 在文件中的相对位置为 (i + 0.5) / 10。
                        position_ratio=(needle_index + 0.5) / NEEDLES_PER_REPO,
                        needle_token_start=int(needle.get("start_byte", 0) or 0),
                        needle_token_end=int(needle.get("end_byte", 0) or 0),
                        repo_index=repo_index,
                        needle_index=needle_index,
                        repo_slug=repo_slug(repo["repo"]),
                        entrypoint_path=repo.get("entrypoint_path", ""),
                        commit_sha=repo.get("commit_sha", ""),
                    )
                )
    return tasks


# ---------------------------------------------------------------------------
# 仓库快照落盘
# ---------------------------------------------------------------------------


def _safe_relative(path: str) -> PurePosixPath:
    """校验数据集里的相对路径，拒绝绝对路径与任何 ``..`` 段（防路径穿越）。"""
    pure = PurePosixPath(path)
    if pure.is_absolute() or (len(path) > 1 and path[1] == ":"):
        raise ValueError(f"拒绝绝对路径：{path!r}")
    if any(part == ".." for part in pure.parts):
        raise ValueError(f"拒绝包含 '..' 的路径：{path!r}")
    if not pure.parts:
        raise ValueError(f"空路径：{path!r}")
    return pure


def materialize_repo(
    repo_entry: dict[str, Any],
    dest_dir: Path | str,
    git_init: bool = True,
    force: bool = False,
) -> tuple[bool, str]:
    """把 ``repo_entry["content"]`` 逐个文件写到 ``dest_dir``，并 ``git init``。

    codeseek 的 ``init`` **没有目标目录参数**，且必须在含 ``.git`` 的目录内运行
    （否则报 ``No project found. Run codeseek from within a git repository.``），
    所以写完文件后必须 ``git init`` 一次。

    :return: ``(是否就绪, 说明)``；``git init`` 失败不算致命（返回 False + 提示，
             调用方仍会让后续 codeseek init 自然失败并给出可读错误）。
    """
    dest = Path(dest_dir)
    manifest_path = dest / MANIFEST_NAME
    expected = {
        "repo": repo_entry.get("repo", ""),
        "commit_sha": repo_entry.get("commit_sha", ""),
        "file_count": len(repo_entry.get("content", {})),
    }

    if not force and manifest_path.is_file():
        try:
            existing = json.loads(manifest_path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            existing = None
        if existing == expected:
            return True, f"已存在快照，跳过 ({expected['file_count']} 文件)"

    written = 0
    for relative_path, text in repo_entry["content"].items():
        pure = _safe_relative(relative_path)
        target = dest.joinpath(*pure.parts)
        target.parent.mkdir(parents=True, exist_ok=True)
        # 用二进制写入，避免 Windows 换行转换破坏行号语义。
        target.write_bytes(text.encode("utf-8"))
        written += 1

    dest.mkdir(parents=True, exist_ok=True)
    manifest_path.write_text(
        json.dumps(expected, ensure_ascii=False, indent=2), encoding="utf-8"
    )

    if not git_init:
        return True, f"写入 {written} 个文件（未执行 git init，仅自检用）"

    try:
        completed = subprocess.run(
            ["git", "init", "-q"],
            cwd=str(dest),
            capture_output=True,
            text=True,
            timeout=120,
            check=False,
        )
    except (FileNotFoundError, subprocess.TimeoutExpired, OSError) as exc:
        return False, f"写入 {written} 个文件，但 git init 失败：{type(exc).__name__}: {exc}"

    if completed.returncode != 0:
        detail = (completed.stderr or completed.stdout or "").strip()[:400]
        return False, f"写入 {written} 个文件，但 git init 返回 {completed.returncode}：{detail}"

    return True, f"写入 {written} 个文件并完成 git init"


# ---------------------------------------------------------------------------
# tasks.jsonl / workdir meta
# ---------------------------------------------------------------------------


def write_tasks(tasks: Iterable[Task], path: Path | str) -> Path:
    """把题目列表写成 jsonl（每题一行），供 run / score 阶段复用。"""
    target = Path(path)
    target.parent.mkdir(parents=True, exist_ok=True)
    with open(target, "w", encoding="utf-8") as handle:
        for task in tasks:
            handle.write(json.dumps(task.to_dict(), ensure_ascii=False) + "\n")
    return target


def load_tasks(path: Path | str) -> list[Task]:
    """读取 ``tasks.jsonl``。"""
    source = Path(path)
    if not source.is_file():
        raise FileNotFoundError(f"题目文件不存在：{source}（先运行 `prepare`）")
    tasks: list[Task] = []
    with open(source, "r", encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if line:
                tasks.append(Task.from_dict(json.loads(line)))
    return tasks


def write_workdir_meta(workdir: Path | str, meta: dict[str, Any]) -> Path:
    """写 ``<workdir>/meta.json``（记录数据集路径与子集口径）。"""
    target = Path(workdir) / WORKDIR_META
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(json.dumps(meta, ensure_ascii=False, indent=2), encoding="utf-8")
    return target


def read_workdir_meta(workdir: Path | str) -> dict[str, Any]:
    """读 ``<workdir>/meta.json``；不存在返回空 dict。"""
    target = Path(workdir) / WORKDIR_META
    if not target.is_file():
        return {}
    try:
        return json.loads(target.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return {}
