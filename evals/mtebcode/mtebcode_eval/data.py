"""MTEB(Code, v1) 数据层：REST 客户端 / 磁盘缓存 / 任务注册表 / 池化抽样 / 语料物化。

数据源（全部为**实测结果**，勿凭印象修改）：

* HuggingFace **datasets-server** 的 JSON REST API，纯标准库 ``urllib.request`` 即可，
  不需要 ``datasets`` / ``huggingface_hub`` / ``pyarrow``。

  - ``GET /splits?dataset=<id>``              枚举 ``(config, split)`` 组合
  - ``GET /rows?dataset=&config=&split=&offset=&length=``  取行，**``length`` 硬上限 100**
  - ``GET /size?dataset=<id>``                数据集规模统计

* 国内网络**直连超时**，必须走代理（``--proxy http://127.0.0.1:7897`` 或 ``https_proxy``）。
* 已知偶发故障：``synthetic-text2sql`` 的 corpus 偶发 502、``CodeSearchNet`` 的 qrels 偶发
  认证错误 —— 都靠指数退避重试（默认 4 次，间隔 1s/2s/4s）兜住。
* 字段名统一：corpus / queries 为 ``_id, text, title, partition, language``
  （``synthetic-text2sql`` 的 queries 额外带 ``context`` = SQL schema）；
  qrels 为 ``query-id, corpus-id, score``（``score`` 为 int，1 表示相关）。
* **主键兼容**：官方 MTEB 数据集（如 ``mteb/CodeSearchNetRetrieval``）用 ``id`` 而不是 ``_id``，
  qrels 也可能用 ``query_id`` / ``corpus_id``。因此所有读主键的地方都走 :func:`_row_doc_id`
  （``row["id"] or row["_id"]``），qrels 走 ``query-id`` → ``query_id`` 的兜底。

池化抽样（与 repoqa 的「100 题子集」哲学一致，但 MTEB 的 corpus 有 2 万~28 万文档，
不可能全量索引）：

1. 拉 qrels 的评测 split，得到 ``query_id -> {corpus_id: score}``（只保留 ``score > 0``）；
2. query_id 排序后按固定 seed 抽样 ``--limit-queries`` 条；
   **分片任务**（``TaskSpec.shard_index/shard_total`` 非空）改为把候选集连续均分成 N 片，
   本任务只取其中一片（见 :func:`shard_query_ids`）；
3. corpus 池 = 抽样 query 的**全部 gold doc** ∪ 从 corpus 里随机抽的干扰文档，直到
   ``--corpus-size``；
4. 只物化池内文档到 ``<workdir>/corpora/<corpus_key 或 task_slug>/docs/``，并写 ``docmap.json``
   （``{相对路径: doc_id}``）供打分时反查 —— **打分不猜路径**；
5. ``git init``（``codeseek init`` 的硬前置），写 ``tasks.jsonl`` + ``meta.json``。

**例外：``TaskSpec.full_corpus=True``（100 个 CodeSearchNetRetrieval 分片）跳过第 3/4 步的池化**，
改为按 corpus 原始顺序物化**全部**行 —— 同语言的 1,000 文档被该语言所有分片共享，因此
``docs/`` 与 ``docmap.json`` 逐字节一致，只需一次 ``codeseek init``（3 个语言 = 3 次 init）。

关于「边拉边筛、命中够了就提前停」：coIR 系列数据集的 ``_id`` 呈位置规律
（实测 cosqa 的 offset 0 → ``d1, d2, d3``，即 ``id = prefix + (行号 + 1)``；
CodeSearchNet python 的 offset 1000 → ``c1000``，即 ``id = prefix + 行号``），
所以先探测 2~3 个 offset 验证该规律，命中则可**按 id 直接算出页偏移**，只拉含 gold doc
的那几页；规律不匹配（例如 ``mteb/CodeEditSearchRetrieval`` 用 hash 当 ``_id``）时
回退到「顺序分页扫描 + 命中够了提前停」。两条路径都记进 ``meta.json``，便于复现。
"""

from __future__ import annotations

import hashlib
import json
import os
import random
import re
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from collections import OrderedDict
from dataclasses import asdict, dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Iterable, Sequence

__all__ = [
    "CSN_SHARD_LANGS",
    "CSN_TASK_NAMES",
    "DEFAULT_TASKS",
    "PAGE_SIZE",
    "PrepareOptions",
    "PreparedTask",
    "Task",
    "TaskSpec",
    "WORKDIR_META",
    "DatasetClient",
    "DatasetError",
    "TASKS",
    "corpus_dir",
    "diff_postimage",
    "load_tasks",
    "prepare_task",
    "read_workdir_meta",
    "resolve_layout",
    "resolve_qrels_split",
    "safe_doc_filename",
    "shard_query_ids",
    "task_slug",
    "write_tasks",
    "write_workdir_meta",
]

# ---------------------------------------------------------------------------
# 常量
# ---------------------------------------------------------------------------

#: datasets-server 基地址。
DATASETS_SERVER = "https://datasets-server.huggingface.co"

#: ``/rows`` 的 ``length`` 硬上限（超过返回 400 ``Parameter 'length' must not be greater than 100``）。
PAGE_SIZE = 100

#: 部分接口在无 UA 时行为不稳定，统一带上。
USER_AGENT = "codeseek-mtebcode-eval/1.0 (+https://github.com/CoIR-team/coir)"

#: workdir 内的固定名字（与 repoqa 保持一致的风格）。
TASKS_FILE = "tasks.jsonl"
RAW_DIR = "raw"
CACHE_DIR = "cache"
CORPORA_DIR = "corpora"
DOCS_SUBDIR = "docs"
DOCMAP_NAME = "docmap.json"
WORKDIR_META = "meta.json"
REPORT_JSON = "report.json"
REPORT_MD = "report.md"

#: 报告里统一的 benchmark 名称。
BENCHMARK = "MTEB(Code, v1)"

#: 代码文件首行 title 注释的前缀（按扩展名取；``.py`` 若写 ``//`` 会破坏语法，
#: 反而让 codeseek 的语法解析失效，因此这里按语言给正确注释符）。
COMMENT_PREFIX: dict[str, str] = {
    ".py": "#",
    ".java": "//",
    ".js": "//",
    ".ts": "//",
    ".go": "//",
    ".rs": "//",
    ".cpp": "//",
    ".rb": "#",
    ".php": "//",
}

#: ``_id`` 位置规律的探查正则（``d123`` / ``c1000`` / ``q42`` 这类 ``前缀 + 数字``）。
_ID_NUMBER_RE = re.compile(r"^(?P<prefix>.*?)(?P<num>\d+)$")


def _utc_now() -> str:
    return datetime.now(tz=timezone.utc).isoformat(timespec="seconds")


def _log(message: str) -> None:
    print(message, file=sys.stderr, flush=True)


def _row_doc_id(row: dict[str, Any]) -> str:
    """行主键：官方 MTEB 数据集用 ``id``，CoIR 系列用 ``_id`` —— 两者都兼容。

    返回空串表示该行没有主键（调用方按「跳过」处理）。**所有**读 corpus / queries 主键的
    地方都必须走这里，否则 ``mteb/CodeSearchNetRetrieval``（只有 ``id``）会整个读空。
    """
    return str(row.get("id") or row.get("_id") or "")


class DatasetError(RuntimeError):
    """数据源相关错误（下载失败 / config-split 不存在 / qrels 为空等）。"""


# ---------------------------------------------------------------------------
# 任务注册表
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class Layout:
    """一个 HF dataset 里三个 config/split 三元组（corpus / queries / qrels）。"""

    corpus_config: str
    corpus_split: str
    queries_config: str
    queries_split: str
    qrels_config: str
    qrels_splits: tuple[str, ...] = ("test", "valid", "train")


@dataclass(frozen=True)
class TaskSpec:
    """MTEB 任务定义。

    :param unverified: ``True`` 表示该条目的 config/split 未实测确认，``--tasks`` 选到它时
        报明确错误而不是崩溃（注册表保留条目，便于后续补测）。
    :param excluded_reason: 非空表示该任务**已实测剔除**（query 过长 / corpus 不可索引等），
        ``prepare`` 选到它时直接报错并回显原因，避免哪天又悄悄跑出来污染批次对比。
    :param query_quota: 每任务独立的 query 配额，覆盖全局 ``--limit-queries``。
        用于「某任务题数与其他任务不同」的补足规则（例如 CosQA 40 + 3×20）。
    :param diff_postimage: ``True`` 表示 corpus 原文是 unified diff，物化时先规整成
        **后像代码**（丢弃 ``@@``/``---``/``+++`` 头与 ``-`` 删除行，去掉 ``+``/空格 的
        1 字符前缀）。实测：30 篇 diff 直接落成 ``.py`` 只被 codeseek 索引出 9 文件/11 符号，
        规整后 17 文件/80 符号 —— 不规整则 .py 语料几乎无法被 tree-sitter 解析。
    :param corpus_key: 非空时作为**共享语料**的目录键：同键的多个任务落到同一个
        ``<workdir>/corpora/<corpus_key>``，从而复用同一次 ``codeseek init``
        （100 个 CSN 分片只需 3 个语料目录 = 3 次 init）。
    :param full_corpus: ``True`` 时物化该 corpus config 的**全部**行，不按 ``--corpus-size``
        裁剪、不随机抽干扰文档。共享语料正确性的前提：同语言各分片必须看到完全相同的
        ``docs/`` 与 ``docmap.json``。
    :param shard_index: 确定性分片：本任务取第几片（0-based）。
    :param shard_total: 确定性分片：该语言共几片。与 ``shard_index`` 同时非空才生效，
        切片口径见 :func:`shard_query_ids`。
    """

    name: str
    hf_dataset: str
    layout: Layout | None
    doc_ext: str = ".txt"
    code_like: bool = False
    language: str = "text"
    note: str = ""
    unverified: bool = False
    excluded_reason: str = ""
    query_quota: int | None = None
    diff_postimage: bool = False
    corpus_key: str = ""
    full_corpus: bool = False
    shard_index: int | None = None
    shard_total: int | None = None

    @property
    def slug(self) -> str:
        return task_slug(self.name)


def _lang_layout(
    language: str,
    corpus_split: str = "corpus",
    queries_split: str = "queries",
    qrels_splits: tuple[str, ...] = ("test", "valid", "train"),
) -> Layout:
    """``CodeSearchNet`` 系列的多语言子 config 命名：``<lang>-corpus`` 等。"""
    return Layout(
        corpus_config=f"{language}-corpus",
        corpus_split=corpus_split,
        queries_config=f"{language}-queries",
        queries_split=queries_split,
        qrels_config=f"{language}-qrels",
        qrels_splits=qrels_splits,
    )


#: ``corpus`` / ``queries`` / ``default`` 三件套（CoIR 里绝大多数任务都是这个形状）。
_DEFAULT_LAYOUT = Layout("corpus", "corpus", "queries", "queries", "default", ("test", "valid", "train"))


#: **任务注册表**。下列 CoIR 条目是本机逐条实测过的历史任务集；默认集见
#: :data:`DEFAULT_TASKS`（只有 CosQA），100 个 CodeSearchNetRetrieval 语言分片在文件末尾
#: 由 :func:`_csn_shard_specs` 批量注册。
#:
#: config/split 全部于本机实测确认（``/splits`` 接口逐条核对），因此 ``unverified`` 均为
#: ``False``；``unverified`` 机制保留给以后新增、尚未探测的任务。
TASKS: "OrderedDict[str, TaskSpec]" = OrderedDict(
    (
        (
            "CosQA",
            TaskSpec(
                name="CosQA",
                hf_dataset="CoIR-Retrieval/cosqa",
                layout=Layout("corpus", "corpus", "queries", "queries", "default", ("test", "valid", "train")),
                doc_ext=".py",
                code_like=True,
                language="python",
                # 第 2 批（全短 query）的补足规则：可用语言只有 3 种 → CosQA 加到 40，
                # 其余 3×20，合计 100 题。全局 --limit-queries 无法表达「每任务不同」，
                # 因此配额下沉到注册表。
                query_quota=40,
                note="NL -> Python code; qrels test has 500 rows (matches MTEB's 500 queries).",
            ),
        ),
        (
            "SyntheticText2SQL",
            TaskSpec(
                name="SyntheticText2SQL",
                hf_dataset="CoIR-Retrieval/synthetic-text2sql",
                layout=_DEFAULT_LAYOUT,
                doc_ext=".sql",
                code_like=False,  # 不往 .sql 里塞注释（spec：不要污染 .sql/.txt）
                language="sql",
                excluded_reason="corpus not indexable (.sql is outside codeseek's extension whitelist)",
                note="NL -> SQL; queries carry a `context` schema appended to the query text; qrels test 5,851 rows.",
            ),
        ),
        (
            "CodeFeedbackST",
            TaskSpec(
                name="CodeFeedbackST",
                hf_dataset="CoIR-Retrieval/codefeedback-st",
                layout=_DEFAULT_LAYOUT,
                language="text",
                excluded_reason="query too long (median 540 chars)",
                note="single-turn code feedback; corpus 156,526 rows.",
            ),
        ),
        (
            "CodeFeedbackMT",
            TaskSpec(
                name="CodeFeedbackMT",
                hf_dataset="CoIR-Retrieval/codefeedback-mt",
                layout=_DEFAULT_LAYOUT,
                language="text",
                excluded_reason="query too long (median 3,571 chars)",
                note="multi-turn code feedback; corpus 66,383 rows.",
            ),
        ),
        (
            "AppsRetrieval",
            TaskSpec(
                name="AppsRetrieval",
                hf_dataset="CoIR-Retrieval/apps",
                layout=_DEFAULT_LAYOUT,
                doc_ext=".py",
                code_like=True,
                language="python",
                excluded_reason="query too long (median 1,316 chars); 98.9% of queries exceed 200 chars",
                note=(
                    "competitive-programming solutions; corpus 8,765 rows. "
                    "doc_ext is .py (not .txt): codeseek's indexer only picks up "
                    "cpp/py/java/rs/ts/tsx/js/go, so .txt corpora index as 0 files."
                ),
            ),
        ),
        (
            "StackOverflowQA",
            TaskSpec(
                name="StackOverflowQA",
                hf_dataset="CoIR-Retrieval/stackoverflow-qa",
                layout=_DEFAULT_LAYOUT,
                language="text",
                excluded_reason="query too long (median 883 chars); 34.2% of query text is natural language",
                note="question -> answer post; corpus 19,931 rows.",
            ),
        ),
        (
            "CodeTransOceanContest",
            TaskSpec(
                name="CodeTransOceanContest",
                hf_dataset="CoIR-Retrieval/codetrans-contest",
                layout=_DEFAULT_LAYOUT,
                doc_ext=".cpp",
                code_like=True,
                language="cpp",
                excluded_reason="query too long (median 547 chars)",
                note=(
                    "contest-level code translation pairs; corpus 1,008 rows. "
                    "corpus text is C++ source, so it is materialized as .cpp "
                    "(codeseek does not index .txt)."
                ),
            ),
        ),
        (
            "CodeTransOceanDL",
            TaskSpec(
                name="CodeTransOceanDL",
                hf_dataset="CoIR-Retrieval/codetrans-dl",
                layout=_DEFAULT_LAYOUT,
                doc_ext=".py",
                code_like=True,
                language="python",
                excluded_reason="query too long (median 1,211 chars)",
                note=(
                    "deep-learning framework API translation pairs; corpus 816 rows. "
                    "corpus text is Python source, so it is materialized as .py "
                    "(codeseek does not index .txt)."
                ),
            ),
        ),
        (
            "CodeSearchNetCCRetrieval",
            TaskSpec(
                name="CodeSearchNetCCRetrieval",
                hf_dataset="CoIR-Retrieval/CodeSearchNet-ccr",
                layout=_lang_layout("python"),
                doc_ext=".py",
                code_like=True,
                language="python",
                note="code -> code (python sub-config) on CodeSearchNet.",
            ),
        ),
    )
)

# ---------------------------------------------------------------------------
# CodeSearchNetRetrieval：100 个语言分片（mteb/CodeSearchNetRetrieval）
# ---------------------------------------------------------------------------

#: ``(语言, 文件扩展名, 分片数)`` —— go 34 + java 33 + python 33 = **100 个任务**。
#:
#: 只评测这三种语言：javascript / php 按需求排除；ruby 因 ``.rb`` 不在 codeseek 的扩展名
#: 白名单（``rust-core/src/codegraph/parser.rs::is_supported_file``）里，物化后会被索引成
#: 0 文件，故排除。
CSN_SHARD_LANGS: tuple[tuple[str, str, int], ...] = (
    ("go", ".go", 34),
    ("java", ".java", 33),
    ("python", ".py", 33),
)


def _csn_layout(language: str) -> Layout:
    """``mteb/CodeSearchNetRetrieval`` 的布局：三个 config 的 split **全部是** ``test``。"""
    return _lang_layout(language, corpus_split="test", queries_split="test", qrels_splits=("test",))


def _csn_shard_specs() -> tuple[tuple[str, TaskSpec], ...]:
    """批量生成 100 个分片任务（避免手写 100 段重复字面量）。

    切分口径：每个语言的 1,000 条 query（qrels 的 query_id 排序后）被**连续均分**给该语言的
    所有分片（见 :func:`shard_query_ids`），因此 100 片的并集 = 全部 3,000 条 query、片与片
    互不重叠；同一语言的 1,000 个 corpus 文档被该语言所有分片共享（``corpus_key`` 相同 +
    ``full_corpus`` 全量物化），运行时只有 3 个语料目录、3 次 ``codeseek init``。
    """
    specs: list[tuple[str, TaskSpec]] = []
    for language, doc_ext, total in CSN_SHARD_LANGS:
        for index in range(total):
            name = f"CodeSearchNetRetrieval-{language}-s{index + 1:02d}"
            specs.append(
                (
                    name,
                    TaskSpec(
                        name=name,
                        hf_dataset="mteb/CodeSearchNetRetrieval",
                        layout=_csn_layout(language),
                        doc_ext=doc_ext,
                        # title 全为空串 -> 不需要往代码里插注释（保持与官方 corpus 文本一致）
                        code_like=False,
                        language=language,
                        # 0 = 跳过 --limit-queries 抽样，让每片从该语言全部 1,000 条 query 里切
                        query_quota=0,
                        corpus_key=f"csn-{language}",
                        full_corpus=True,
                        shard_index=index,
                        shard_total=total,
                        note=(
                            f"{language} shard {index + 1:02d}/{total} of mteb/CodeSearchNetRetrieval: "
                            f"a contiguous slice of the same sorted 1,000-query list "
                            f"(~{1000 // total} queries per shard); all {total} shards share "
                            f"corpora/csn-{language} (1,000 corpus docs, one `codeseek init`)."
                        ),
                    ),
                )
            )
    return tuple(specs)


_CSN_SHARD_SPECS: tuple[tuple[str, TaskSpec], ...] = _csn_shard_specs()

TASKS.update(_CSN_SHARD_SPECS)

#: 100 个分片任务名（供 CLI 的 ``--tasks csn`` 别名与自检复用）。
CSN_TASK_NAMES: tuple[str, ...] = tuple(name for name, _spec in _CSN_SHARD_SPECS)

#: 默认评测集：只留一条**必然可跑**的冒烟任务（NL → code）。
#:
#: 历史值是 ``("CosQA", "CodeSearchNetRetrieval", "SyntheticText2SQL")``，后两条都带
#: ``excluded_reason``，导致不带 ``--tasks`` 跑 ``prepare`` 直接抛 DatasetError。
#: 100 个 CSN 分片体量大，不适合当默认值；用 ``--tasks csn`` 一键选中。
DEFAULT_TASKS: tuple[str, ...] = ("CosQA",)


def task_slug(name: str) -> str:
    """``SyntheticText2SQL`` → ``synthetictext2sql``（目录名 / task_id 前缀）。"""
    return re.sub(r"[^a-z0-9]+", "", name.lower())


def corpus_dir(workdir: Path | str, spec_or_name: "TaskSpec | str") -> Path:
    """语料目录：``<workdir>/corpora/<corpus_key 或 task_slug>``。

    ``TaskSpec.corpus_key`` 非空时优先用它 —— 这是**共享语料**的机制：100 个 CSN 分片里
    同语言的多个任务共用 ``corpora/csn-<lang>``，从而只做一次 ``codeseek init``。
    """
    if isinstance(spec_or_name, TaskSpec):
        key = spec_or_name.corpus_key or spec_or_name.slug
    else:
        key = task_slug(spec_or_name)
    return Path(workdir) / CORPORA_DIR / key


# ---------------------------------------------------------------------------
# REST 客户端：磁盘缓存 + 指数退避重试 + 速率控制 + 代理
# ---------------------------------------------------------------------------


def _env_proxy() -> str | None:
    """默认代理：读 ``https_proxy`` / ``HTTPS_PROXY`` / ``http_proxy`` / ``HTTP_PROXY``。"""
    for key in ("https_proxy", "HTTPS_PROXY", "http_proxy", "HTTP_PROXY"):
        value = os.environ.get(key, "").strip()
        if value:
            return value
    return None


def _proxy_opener(proxy: str | None) -> urllib.request.OpenerDirector:
    """构造 opener（**不改全局** ``urllib.request.install_opener``）。"""
    if proxy:
        handler = urllib.request.ProxyHandler({"http": proxy, "https": proxy})
        return urllib.request.build_opener(handler)
    return urllib.request.build_opener()


def _atomic_write_text(path: Path, text: str) -> None:
    """``.part`` → rename 的原子写（中断不会留下半截 JSON）。"""
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(path.suffix + ".part")
    tmp.write_text(text, encoding="utf-8")
    tmp.replace(path)


@dataclass
class ClientStats:
    """网络访问统计（写进 ``meta.json``，说明这次抓了多少页、命中多少缓存）。"""

    requests: int = 0
    cache_hits: int = 0
    retries: int = 0
    failures: int = 0
    bytes_downloaded: int = 0
    parquet_pages: int = 0

    def to_dict(self) -> dict[str, Any]:
        return asdict(self)


class ParquetRows:
    """本地 HF parquet 缓存读取器：datasets-server 429/500 时的**离线等价数据源**。

    目录布局就是 ``huggingface_hub`` 的默认缓存::

        <cache>/datasets--<org>--<name>/snapshots/<rev>/<config>/<split>-00000-of-00001.parquet

    只实现 ``prepare`` 需要的两个能力 —— ``splits()`` 与 ``rows()`` —— 且返回值与 REST
    的形状**逐字段一致**（``{"rows": [{"row": {...}}, ...], "num_rows_total": N}``），
    因此上层 :func:`prepare_task` 完全不需要分支。

    为什么要它：实测本机对 ``datasets-server`` 的 ``/rows`` 会持续返回 **429 Too Many
    Requests**（重试 6 次仍失败），而同一份数据以 parquet 形式就在本地缓存里。逐 (config, split)
    回退，能把「抓数据」这一步变成确定性的、可离线复现的步骤；用了哪些来源会写进
    ``meta.json.client_stats.parquet_pages`` 与 ``meta.json.data_sources``。
    """

    def __init__(self, root: Path | str, quiet: bool = False) -> None:
        self.root = Path(root).expanduser()
        self.quiet = quiet
        self._datasets: dict[str, Path | None] = {}
        self._tables: dict[str, Any] = {}

    # -- 内部 --------------------------------------------------------------
    def _snapshot(self, dataset: str) -> Path | None:
        if dataset in self._datasets:
            return self._datasets[dataset]
        base = self.root / ("datasets--" + dataset.replace("/", "--"))
        found: Path | None = None
        snapshots_dir = base / "snapshots"
        if snapshots_dir.is_dir():
            for snapshot in sorted(snapshots_dir.iterdir(), reverse=True):
                if snapshot.is_dir() and any(snapshot.glob("*/*.parquet")):
                    found = snapshot
                    break
        self._datasets[dataset] = found
        return found

    def _matches(self, dataset: str, config: str, split: str) -> list[Path]:
        snapshot = self._snapshot(dataset)
        if snapshot is None:
            return []
        directory = snapshot / config
        if not directory.is_dir():
            return []
        return sorted(directory.glob(f"{split}-*.parquet"))

    def has(self, dataset: str, config: str, split: str) -> bool:
        """该 (dataset, config, split) 是否有本地 parquet 可读。"""
        return bool(self._matches(dataset, config, split))

    def _table(self, dataset: str, config: str, split: str) -> Any:
        key = f"{dataset}::{config}::{split}"
        if key in self._tables:
            return self._tables[key]
        matches = self._matches(dataset, config, split)
        if not matches:
            raise DatasetError(f"本地 parquet 缺失：{dataset} {config}/{split}（{self.root}）")
        try:
            import pyarrow.parquet as pq  # 局部导入：只有启用本地源时才需要 pyarrow
        except ImportError as exc:  # pragma: no cover - 环境缺依赖时的明确报错
            raise DatasetError(
                "启用了本地 parquet 数据源但未安装 pyarrow；`pip install pyarrow` 后重试"
            ) from exc
        table = pq.read_table(str(matches[0]))
        self._tables[key] = table
        return table

    # -- 与 REST 同形的接口 -------------------------------------------------
    def splits(self, dataset: str) -> list[dict[str, str]]:
        """枚举本地 parquet 覆盖的 ``(config, split)``。"""
        snapshot = self._snapshot(dataset)
        if snapshot is None:
            return []
        out: list[dict[str, str]] = []
        for parquet in sorted(snapshot.glob("*/*.parquet")):
            split = parquet.name.split("-")[0]
            out.append({"config": parquet.parent.name, "split": split})
        return out

    def num_rows(self, dataset: str, config: str, split: str) -> int:
        return int(self._table(dataset, config, split).num_rows)

    def rows(
        self, dataset: str, config: str, split: str, offset: int, length: int = PAGE_SIZE
    ) -> dict[str, Any]:
        table = self._table(dataset, config, split)
        total = int(table.num_rows)
        start = max(0, int(offset))
        stop = min(total, start + max(0, int(length)))
        rows = table.slice(start, max(0, stop - start)).to_pylist()
        return {
            "rows": [{"row": dict(item)} for item in rows],
            "num_rows_total": total,
            "features": [{"name": key} for key in (table.column_names or [])],
            "source": "local-parquet",
        }


class DatasetClient:
    """datasets-server 的极简客户端。

    * **缓存**：``<workdir>/cache/<dataset>/<config>/<split>/<offset>.json``，重跑直接复用；
    * **重试**：默认 4 次，间隔 1s / 2s / 4s；HTTP 400 视为参数错误，不重试；
    * **速率**：每次**真实**网络请求后 sleep ``rate_limit`` 秒（缓存命中不 sleep）；
    * **本地 parquet 回退**：给出 ``parquet_cache`` 时，若某 ``(config, split)`` 在本地 HF
      缓存里有 parquet，就直接读本地，**不发网络请求**（应对 ``/rows`` 的 429）。
    """

    def __init__(
        self,
        workdir: Path | str,
        proxy: str | None = None,
        retries: int = 4,
        rate_limit: float = 0.15,
        timeout: float = 120.0,
        quiet: bool = False,
        cache_only: bool = False,
        parquet_cache: Path | str | None = None,
    ) -> None:
        self.workdir = Path(workdir)
        self.cache_root = self.workdir / CACHE_DIR
        self.proxy = proxy if proxy is not None else _env_proxy()
        self.retries = max(1, int(retries))
        self.rate_limit = max(0.0, float(rate_limit))
        self.timeout = float(timeout)
        self.quiet = quiet
        self.cache_only = bool(cache_only)
        self.stats = ClientStats()
        self._opener = _proxy_opener(self.proxy)
        self.parquet = ParquetRows(parquet_cache, quiet=quiet) if parquet_cache else None

    # -- 内部：缓存路径 ----------------------------------------------------
    def _cache_base(self, dataset: str) -> Path:
        return self.cache_root / dataset.replace("/", "__")

    def _rows_cache_path(
        self, dataset: str, config: str, split: str, offset: int, length: int = PAGE_SIZE
    ) -> Path:
        """``/rows`` 的磁盘缓存路径。

        **``length`` 必须进文件名。** 历史实现只按 ``offset`` 命名，而
        :func:`probe_id_scheme` 会用 ``length=1`` 探测 ``{0, num_rows//2, num_rows-1}``：
        探测把 ``offset=num_rows//2`` 写成了「只含 1 行」的缓存，随后 :func:`iter_rows`
        请求同一 ``offset`` 的整页（``length=PAGE_SIZE``）时命中它，拿到 1 行就
        ``offset += PAGE_SIZE``，中间 99 行被整段跳过。

        实测后果：``mteb/CodeSearchNetRetrieval`` 的 go/java/python 三个 qrels 表各
        1000 行，``num_rows//2 == 500``，于是三语言都以**完全相同**的区间丢掉
        ``501..599``，每语言只剩 901 条 query（2703 题而非 3000 题）。
        """
        return (
            self._cache_base(dataset)
            / _safe_segment(config)
            / _safe_segment(split)
            / f"{int(offset)}_{int(length)}.json"
        )

    def _meta_cache_path(self, dataset: str, name: str) -> Path:
        return self._cache_base(dataset) / f"_{name}.json"

    # -- 内部：请求 --------------------------------------------------------
    def _fetch_json(self, path: str, params: dict[str, Any]) -> dict[str, Any]:
        """GET ``path`` + 查询参数 → dict，失败时抛出带上下文的 :class:`DatasetError`。"""
        query = urllib.parse.urlencode({k: v for k, v in params.items() if v is not None})
        url = f"{DATASETS_SERVER}{path}?{query}"
        request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})

        last_detail = ""
        for attempt in range(1, self.retries + 1):
            try:
                self.stats.requests += 1
                with self._opener.open(request, timeout=self.timeout) as response:
                    body = response.read()
                self.stats.bytes_downloaded += len(body)
                text = body.decode("utf-8", errors="replace")
                if self.rate_limit:
                    time.sleep(self.rate_limit)
                payload = json.loads(text)
                if isinstance(payload, dict) and payload.get("error"):
                    # datasets-server 有时用 200 + {"error": "..."} 表达失败
                    last_detail = f"server error field: {str(payload.get('error'))[:300]}"
                    raise _RetryableError(last_detail)
                if not isinstance(payload, dict):
                    raise _RetryableError(f"response is {type(payload).__name__}, expected object")
                return payload
            except urllib.error.HTTPError as exc:
                detail = _read_error_body(exc)
                last_detail = f"HTTP {exc.code} {exc.reason}"
                if detail:
                    last_detail += f" — {detail}"
                if exc.code == 400:
                    # 参数错误（例如 length > 100）重试没有意义
                    self.stats.failures += 1
                    raise DatasetError(f"请求失败：{last_detail}\n  URL: {url}") from exc
                if attempt >= self.retries:
                    self.stats.failures += 1
                    raise DatasetError(
                        f"请求失败（已重试 {self.retries} 次）：{last_detail}\n  URL: {url}"
                    ) from exc
            except (_RetryableError, urllib.error.URLError, TimeoutError, OSError, json.JSONDecodeError) as exc:
                last_detail = f"{type(exc).__name__}: {exc}"
                if attempt >= self.retries:
                    self.stats.failures += 1
                    raise DatasetError(
                        f"请求失败（已重试 {self.retries} 次）：{last_detail}\n  URL: {url}"
                    ) from exc

            self.stats.retries += 1
            backoff = 2 ** (attempt - 1)  # 1s / 2s / 4s ...
            if not self.quiet:
                _log(f"[data] 重试 {attempt}/{self.retries - 1}（{backoff}s 后）：{last_detail}")
            time.sleep(backoff)

        raise DatasetError(f"请求失败：{last_detail}\n  URL: {url}")  # pragma: no cover - 兜底

    def _cached_json(self, path: Path, fetch, description: str) -> dict[str, Any]:
        """缓存命中直接返回，否则 ``fetch()`` 拉取并原子落盘。"""
        if path.is_file():
            try:
                payload = json.loads(path.read_text(encoding="utf-8"))
            except (OSError, json.JSONDecodeError):
                payload = None
            if isinstance(payload, dict):
                self.stats.cache_hits += 1
                return payload
        if self.cache_only:
            raise DatasetError(
                f"--cache-only 已开启，但缓存缺失：{path}\n"
                "  去掉 --cache-only 让它联网抓取（必要时先用同一条命令缓存一遍）"
            )
        payload = fetch()
        _atomic_write_text(path, json.dumps(payload, ensure_ascii=False))
        if not self.quiet:
            _log(f"[data] 已缓存 {description}")
        return payload

    # -- 公开接口 ----------------------------------------------------------
    def splits(self, dataset: str) -> list[dict[str, str]]:
        """``GET /splits``：返回 ``[{"config":..., "split":...}, ...]``。

        有本地 parquet 时把它的 ``(config, split)`` **并入**结果：既保留 REST 的完整视图
        （parquet 未下载的 config 仍在），又保证 ``/splits`` 被 429 时仍有可用信息。
        """
        path = self._meta_cache_path(dataset, "splits")

        def _fetch() -> dict[str, Any]:
            return self._fetch_json("/splits", {"dataset": dataset})

        remote: list[dict[str, str]] = []
        try:
            payload = self._cached_json(path, _fetch, f"{dataset} /splits")
            splits = payload.get("splits")
            if not isinstance(splits, list):
                raise DatasetError(f"{dataset}: /splits 返回结构异常：{str(payload)[:300]}")
            remote = [
                {"config": str(item.get("config", "")), "split": str(item.get("split", ""))}
                for item in splits
                if isinstance(item, dict)
            ]
        except DatasetError:
            if self.parquet is None or not self.parquet.splits(dataset):
                raise
            if not self.quiet:
                _log(f"[data] /splits 不可用，改用本地 parquet 的 config/split 视图：{dataset}")

        seen = {(item["config"], item["split"]) for item in remote}
        merged = list(remote)
        if self.parquet is not None:
            for item in self.parquet.splits(dataset):
                key = (item["config"], item["split"])
                if key not in seen:
                    seen.add(key)
                    merged.append(item)
        return merged

    def size(self, dataset: str) -> dict[str, Any]:
        """``GET /size``：数据集规模统计（写进 ``meta.json``）。"""
        path = self._meta_cache_path(dataset, "size")

        def _fetch() -> dict[str, Any]:
            return self._fetch_json("/size", {"dataset": dataset})

        return self._cached_json(path, _fetch, f"{dataset} /size")

    def rows(self, dataset: str, config: str, split: str, offset: int, length: int = PAGE_SIZE) -> dict[str, Any]:
        """``GET /rows``：取一页（``length <= 100``，严格校验）。

        本地 parquet 命中该 ``(config, split)`` 时直接读本地，不发网络请求。
        """
        if not 1 <= int(length) <= PAGE_SIZE:
            raise ValueError(f"length 必须在 1..{PAGE_SIZE} 之间（datasets-server 硬上限），收到 {length}")
        offset = max(0, int(offset))
        if self.parquet is not None and self.parquet.has(dataset, config, split):
            payload = self.parquet.rows(dataset, config, split, offset, length)
            self.stats.parquet_pages += 1
            return payload
        path = self._rows_cache_path(dataset, config, split, offset, length)

        def _fetch() -> dict[str, Any]:
            return self._fetch_json(
                "/rows",
                {"dataset": dataset, "config": config, "split": split, "offset": offset, "length": int(length)},
            )

        def _validated(payload: dict[str, Any]) -> dict[str, Any]:
            rows = payload.get("rows")
            if not isinstance(rows, list):
                raise DatasetError(
                    f"{dataset} {config}/{split} @{offset}: /rows 返回结构异常：{str(payload)[:300]}"
                )
            return payload

        payload = _validated(
            self._cached_json(path, _fetch, f"{dataset} {config}/{split} @{offset}+{int(length)}")
        )
        if _page_short_of_request(payload, offset, int(length)):
            # 缓存页比请求短且不属于「末页」→ 是被更短请求（probe 的 length=1）
            # 或更早版本（缓存名不含 length）污染的坏页。删掉重拉一次，
            # 绝不把缺行当成「表就这么多行」交给上层（那会静默丢评测题）。
            try:
                path.unlink()
            except OSError:
                pass
            payload = _validated(_fetch())
            _atomic_write_text(path, json.dumps(payload, ensure_ascii=False))
            if not self.quiet:
                _log(
                    f"[data] 缓存页行数不足，已重新拉取并覆盖："
                    f"{dataset} {config}/{split} @{offset}+{int(length)}"
                )
        return payload

    def num_rows(self, dataset: str, config: str, split: str) -> int:
        """从第一页读 ``num_rows_total``（顺便把 offset=0 的页缓存下来）。"""
        payload = self.rows(dataset, config, split, 0, min(PAGE_SIZE, PAGE_SIZE))
        total = payload.get("num_rows_total")
        try:
            return int(total)  # type: ignore[arg-type]
        except (TypeError, ValueError):
            rows = payload.get("rows") or []
            return len(rows)


class _RetryableError(RuntimeError):
    """内部标记：可重试的软失败。"""


def _read_error_body(exc: urllib.error.HTTPError, limit: int = 300) -> str:
    try:
        raw = exc.read()
    except Exception:  # noqa: BLE001 - 读取错误体失败不影响主流程
        return ""
    text = raw.decode("utf-8", errors="replace").strip()
    return text[:limit]


def _safe_segment(value: str) -> str:
    """把 config/split 名安全化成单层目录名。"""
    return re.sub(r"[^A-Za-z0-9._-]", "_", value) or "_"


def _page_short_of_request(payload: dict[str, Any], offset: int, length: int) -> bool:
    """判断一页是否「比请求短且不可能是末页」——即被污染的坏缓存。

    末页天然比 ``length`` 短（``offset + 实得行数 == num_rows_total``），不算坏页；
    只有当「请求区间整体落在表内」（``offset + length <= num_rows_total``）却拿到
    更少的行时，才说明这页缓存的内容属于一次更短的请求。

    无法读到 ``num_rows_total`` 时保守判为坏页：宁可多拉一次，也不能静默丢行。
    """
    rows = payload.get("rows")
    if not isinstance(rows, list) or len(rows) >= length:
        return False
    try:
        total = int(payload.get("num_rows_total"))  # type: ignore[arg-type]
    except (TypeError, ValueError):
        return True
    return offset + length <= total


# ---------------------------------------------------------------------------
# config / split 解析
# ---------------------------------------------------------------------------


def detect_layout(splits: Sequence[dict[str, str]]) -> Layout | None:
    """从 ``/splits`` 里推断 ``corpus / queries / qrels`` 三件套（注册表未验证时使用）。"""
    by_config: dict[str, list[str]] = {}
    for item in splits:
        by_config.setdefault(item["config"], []).append(item["split"])

    corpus_candidates = [c for c in by_config if c == "corpus" or c.endswith("-corpus")]
    queries_candidates = [c for c in by_config if c == "queries" or c.endswith("-queries")]

    def _prefer(candidates: Sequence[str], exact: str) -> str | None:
        if not candidates:
            return None
        if exact in candidates:
            return exact
        return sorted(candidates)[0]

    corpus_config = _prefer(corpus_candidates, "corpus")
    queries_config = _prefer(queries_candidates, "queries")
    if not corpus_config or not queries_config:
        return None
    corpus_split = "corpus" if "corpus" in by_config[corpus_config] else by_config[corpus_config][0]
    queries_split = "queries" if "queries" in by_config[queries_config] else by_config[queries_config][0]

    qrels_config = None
    qrels_splits: tuple[str, ...] = ()
    ordered = ("test", "valid", "train")
    for config, available in by_config.items():
        if config in (corpus_config, queries_config):
            continue
        hits = tuple(split for split in ordered if split in available)
        if hits:
            qrels_config = config
            qrels_splits = hits
            if config == "default":
                break
    if not qrels_config:
        return None
    return Layout(corpus_config, corpus_split, queries_config, queries_split, qrels_config, qrels_splits)


def resolve_layout(client: DatasetClient, spec: TaskSpec) -> Layout:
    """确认注册表里的 config/split 真实存在；未注册的走 ``detect_layout`` 探测。

    :raises DatasetError: 探测失败 —— 错误信息里列出该数据集可用的 config，可读不崩溃。
    """
    available = client.splits(spec.hf_dataset)
    pairs = {(item["config"], item["split"]) for item in available}

    layout = spec.layout
    if layout is None:
        detected = detect_layout(available)
        if detected is None:
            raise DatasetError(
                f"{spec.name}: 无法从 {spec.hf_dataset} 的 /splits 推断出 corpus/queries/qrels 布局；"
                f"可用 config：{sorted({item['config'] for item in available})}"
            )
        return detected

    required = [
        (layout.corpus_config, layout.corpus_split),
        (layout.queries_config, layout.queries_split),
    ]
    missing = [pair for pair in required if pair not in pairs]
    if missing:
        raise DatasetError(
            f"{spec.name}: {spec.hf_dataset} 上不存在 config/split {missing}；"
            f"可用组合：{sorted(pairs)[:20]}"
        )
    if not any((layout.qrels_config, split) in pairs for split in layout.qrels_splits):
        raise DatasetError(
            f"{spec.name}: {spec.hf_dataset} 上不存在 qrels config={layout.qrels_config} 且 split ∈ "
            f"{list(layout.qrels_splits)}；可用组合：{sorted(pairs)[:20]}"
        )
    return layout


def resolve_qrels_split(client: DatasetClient, spec: TaskSpec, layout: Layout) -> str:
    """qrels split 选择规则：优先 ``test``，其次 ``valid``，最后 ``train``。"""
    pairs = {(item["config"], item["split"]) for item in client.splits(spec.hf_dataset)}
    for split in layout.qrels_splits:
        if (layout.qrels_config, split) in pairs:
            return split
    raise DatasetError(
        f"{spec.name}: {spec.hf_dataset} 的 qrels config={layout.qrels_config} 没有可用的 split"
        f"（尝试过 {list(layout.qrels_splits)}）"
    )


# ---------------------------------------------------------------------------
# 分页遍历 / 按 id 取行 / 随机抽样
# ---------------------------------------------------------------------------


@dataclass
class FetchStats:
    """一次「按 id 取行」的统计（写进 ``meta.json``，说明扫了多少页）。"""

    mode: str = "scan"
    pages: int = 0
    rows_scanned: int = 0
    missing: list[str] = field(default_factory=list)

    def to_dict(self) -> dict[str, Any]:
        return asdict(self)


def iter_rows(
    client: DatasetClient,
    dataset: str,
    config: str,
    split: str,
    num_rows: int,
    start_offset: int = 0,
) -> Iterable[list[dict[str, Any]]]:
    """按 ``PAGE_SIZE`` 分页遍历，逐页 yield ``row`` 字典列表。"""
    offset = max(0, int(start_offset))
    while offset < num_rows:
        length = min(PAGE_SIZE, num_rows - offset)
        payload = client.rows(dataset, config, split, offset, length)
        page = [item.get("row") or {} for item in (payload.get("rows") or []) if isinstance(item, dict)]
        if not page:
            return
        yield page
        offset += PAGE_SIZE


def probe_id_scheme(
    client: DatasetClient, dataset: str, config: str, split: str, num_rows: int
) -> tuple[str, int] | None:
    """探测 ``_id`` 是否为 ``prefix + (行号 + delta)``。

    :return: ``(prefix, delta)``；无法确认（id 是 hash、或探测点不一致）时返回 ``None``。
    """
    if num_rows < 2:
        return None
    probes = sorted({0, num_rows // 2, num_rows - 1})
    observations: list[tuple[str, int, int]] = []
    for offset in probes:
        payload = client.rows(dataset, config, split, offset, 1)
        rows = payload.get("rows") or []
        if not rows:
            return None
        doc_id = _row_doc_id(rows[0].get("row") or {})
        match = _ID_NUMBER_RE.match(doc_id)
        if not match:
            return None
        observations.append((match.group("prefix"), int(match.group("num")), offset))
    prefixes = {item[0] for item in observations}
    deltas = {item[1] - item[2] for item in observations}
    if len(prefixes) != 1 or len(deltas) != 1:
        return None
    return prefixes.pop(), deltas.pop()


def _scan_rows(
    client: DatasetClient,
    dataset: str,
    config: str,
    split: str,
    want_ids: Sequence[str],
    num_rows: int,
) -> tuple[dict[str, dict[str, Any]], FetchStats]:
    """顺序分页扫描，**命中够了就提前停**（不强求扫完全部 corpus）。"""
    stats = FetchStats(mode="scan")
    wanted = set(want_ids)
    found: dict[str, dict[str, Any]] = {}
    offset = 0
    while offset < num_rows and len(found) < len(wanted):
        length = min(PAGE_SIZE, num_rows - offset)
        payload = client.rows(dataset, config, split, offset, length)
        stats.pages += 1
        rows = [item.get("row") or {} for item in (payload.get("rows") or []) if isinstance(item, dict)]
        stats.rows_scanned += len(rows)
        if not rows:
            break
        for row in rows:
            doc_id = _row_doc_id(row)
            if doc_id in wanted and doc_id not in found:
                found[doc_id] = row
        offset += PAGE_SIZE
    stats.missing = sorted(wanted - set(found))
    return found, stats


def fetch_rows_by_ids(
    client: DatasetClient,
    dataset: str,
    config: str,
    split: str,
    ids: Sequence[str],
    num_rows: int,
) -> tuple[dict[str, dict[str, Any]], FetchStats]:
    """按 ``_id`` 取行。

    两条路径：

    1. **positional**：``_id`` 呈 ``prefix + (行号 + delta)`` 规律时，直接算出页偏移，
       只拉含目标文档的那几页（避免扫完 28 万行）；
    2. **scan**：规律不匹配时顺序分页扫描 + 命中够了提前停。

    路径与页数都记进 :class:`FetchStats`。
    """
    wanted = list(dict.fromkeys(str(item) for item in ids))
    if not wanted:
        return {}, FetchStats(mode="skip")

    scheme = probe_id_scheme(client, dataset, config, split, num_rows)
    if scheme is not None:
        prefix, delta = scheme
        pages: "dict[int, list[str]]" = {}
        resolvable = True
        for doc_id in wanted:
            match = _ID_NUMBER_RE.match(doc_id)
            if not match or match.group("prefix") != prefix:
                resolvable = False
                break
            index = int(match.group("num")) - delta
            if index < 0 or index >= num_rows:
                resolvable = False
                break
            pages.setdefault((index // PAGE_SIZE) * PAGE_SIZE, []).append(doc_id)

        if resolvable:
            stats = FetchStats(mode="positional")
            found: dict[str, dict[str, Any]] = {}
            for page_offset in sorted(pages):
                length = min(PAGE_SIZE, num_rows - page_offset)
                payload = client.rows(dataset, config, split, page_offset, length)
                stats.pages += 1
                rows = [item.get("row") or {} for item in (payload.get("rows") or []) if isinstance(item, dict)]
                stats.rows_scanned += len(rows)
                by_id = {_row_doc_id(row): row for row in rows}
                for doc_id in pages[page_offset]:
                    row = by_id.get(doc_id)
                    if row is not None:
                        found[doc_id] = row
            if len(found) == len(wanted):
                return found, stats

            # 位置规律命中不全（极少见）：用扫描补齐剩余 id
            missing_ids = [doc_id for doc_id in wanted if doc_id not in found]
            extra, extra_stats = _scan_rows(client, dataset, config, split, missing_ids, num_rows)
            stats.pages += extra_stats.pages
            stats.rows_scanned += extra_stats.rows_scanned
            found.update(extra)
            stats.mode = "positional+scan"
            stats.missing = [doc_id for doc_id in wanted if doc_id not in found]
            return found, stats

    found, stats = _scan_rows(client, dataset, config, split, wanted, num_rows)
    return found, stats


def sample_rows(
    client: DatasetClient,
    dataset: str,
    config: str,
    split: str,
    num_rows: int,
    count: int,
    seed: int,
    exclude: set[str],
    max_pages: int = 0,
) -> tuple[list[dict[str, Any]], FetchStats]:
    """按固定 seed **确定性**随机抽 ``count`` 行干扰文档。

    ``exclude`` 会被原地更新（保证结果里没有重复 doc / 没有 gold doc）。
    """
    stats = FetchStats(mode="random")
    if count <= 0 or num_rows <= 0:
        return [], stats

    rng = random.Random(_stable_seed(seed, dataset, config, split))
    page_starts = list(range(0, num_rows, PAGE_SIZE))
    budget = max_pages if max_pages > 0 else len(page_starts)
    picked: list[dict[str, Any]] = []
    visited: set[int] = set()
    attempts = 0

    while len(picked) < count and attempts < budget and len(visited) < len(page_starts):
        attempts += 1
        page_offset = page_starts[rng.randrange(len(page_starts))]
        if page_offset in visited:
            continue
        visited.add(page_offset)
        length = min(PAGE_SIZE, num_rows - page_offset)
        payload = client.rows(dataset, config, split, page_offset, length)
        stats.pages += 1
        rows = [item.get("row") or {} for item in (payload.get("rows") or []) if isinstance(item, dict)]
        stats.rows_scanned += len(rows)
        for row in rows:
            doc_id = _row_doc_id(row)
            if not doc_id or doc_id in exclude:
                continue
            exclude.add(doc_id)
            picked.append(row)
            if len(picked) >= count:
                break
    if len(picked) < count:
        stats.missing = [f"short_by_{count - len(picked)}"]
    return picked, stats


def _stable_seed(*parts: Any) -> int:
    """跨进程稳定的 seed（``hash()`` 对 str 加了随机盐，不能用）。"""
    digest = hashlib.sha1("::".join(str(part) for part in parts).encode("utf-8")).hexdigest()
    return int(digest[:16], 16)


# ---------------------------------------------------------------------------
# qrels
# ---------------------------------------------------------------------------


def fetch_qrels(client: DatasetClient, dataset: str, config: str, split: str) -> tuple[dict[str, dict[str, float]], int]:
    """拉完整 qrels → ``{query_id: {doc_id: score}}``（只保留 ``score > 0``）。

    键名兼容两套写法：MTEB 官方（``mteb/CodeSearchNetRetrieval``）与 datasets-server
    导出的 CoIR 数据都用连字符 ``query-id`` / ``corpus-id``，但下划线写法 ``query_id`` /
    ``corpus_id`` 也一并兜住，避免换个数据源就整表读空。

    :return: ``(qrels, 读取的行数)``
    """
    num_rows = client.num_rows(dataset, config, split)
    qrels: dict[str, dict[str, float]] = {}
    read = 0
    for page in iter_rows(client, dataset, config, split, num_rows):
        for row in page:
            read += 1
            query_id = str(row.get("query-id") or row.get("query_id") or "").strip()
            doc_id = str(row.get("corpus-id") or row.get("corpus_id") or "").strip()
            if not query_id or not doc_id:
                continue
            try:
                score = float(row.get("score") or 0)
            except (TypeError, ValueError):
                score = 0.0
            if score <= 0:
                continue
            qrels.setdefault(query_id, {})[doc_id] = score
    return qrels, read


def sample_query_ids(query_ids: Sequence[str], limit: int, seed: int, task_name: str) -> list[str]:
    """query_id 排序后按固定 seed 抽样（确定性，重跑一致）。"""
    ordered = sorted(set(query_ids))
    if limit <= 0 or limit >= len(ordered):
        return ordered
    rng = random.Random(_stable_seed(seed, task_name, "queries"))
    picked = rng.sample(ordered, limit)
    return sorted(picked)


def shard_query_ids(query_ids: Sequence[str], index: int, total: int) -> list[str]:
    """把排序去重后的 query_id **连续均分**成 ``total`` 片，返回第 ``index`` 片（0-based）。

    互斥且并集为全集：前 ``len(query_ids) % total`` 片各多 1 条（1,000 条 / 34 片 →
    前 16 片 30 条、其余 18 片 29 条）。纯函数、不依赖网络与随机数，便于单测。

    :raises ValueError: ``index`` 越界，或 ``total <= 0``。
    """
    ordered = sorted(set(str(item) for item in query_ids))
    total = int(total)
    if total <= 0:
        raise ValueError(f"shard total 必须 >= 1，收到 {total}")
    if total == 1:
        return ordered
    index = int(index)
    if index < 0 or index >= total:
        raise ValueError(f"shard index 必须在 0..{total - 1} 之间，收到 {index}")
    base, extra = divmod(len(ordered), total)
    start = index * base + min(index, extra)
    stop = start + base + (1 if index < extra else 0)
    return ordered[start:stop]


# ---------------------------------------------------------------------------
# 语料物化
# ---------------------------------------------------------------------------


def safe_doc_filename(doc_id: str, ext: str) -> str:
    """doc_id → 文件系统安全的名字：非法字符替换 + 短哈希防碰撞。"""
    stem = re.sub(r"[^A-Za-z0-9._-]", "_", doc_id).strip("_") or "doc"
    if len(stem) > 80:
        stem = stem[:80]
    digest = hashlib.sha1(doc_id.encode("utf-8")).hexdigest()[:8]
    return f"{stem}_{digest}{ext}"


def diff_postimage(text: str) -> str:
    """unified diff → **后像代码**（``mteb/CodeEditSearchRetrieval`` 的 corpus 全是 diff）。

    规则（只做「去补丁记号」，不改动任何代码字符）：

    * 丢弃 ``@@ ... @@`` hunk 头、``---``/``+++`` 文件头、``\\ No newline`` 标记；
    * ``+`` 行 → 去掉首个 ``+``（新增代码，保留）；
    * ``（空格）`` 行 → 去掉首个空格（上下文行，保留）；
    * ``-`` 行 → 丢弃（被删除的旧代码，不属于后像）。

    为什么必须做：实测同一批 30 篇 python corpus，直接落成 ``.py`` 时 codeseek 只索引出
    **9 文件 / 11 符号**，规整后 **17 文件 / 80 符号** —— diff 记号会让 tree-sitter 大面积解析失败。
    该转换只在 ``TaskSpec.diff_postimage=True`` 时启用，并写进 ``meta.json`` 的
    ``materialization`` 字段，保证可审计。
    """
    kept: list[str] = []
    for line in text.splitlines():
        if line.startswith("@@"):
            continue
        if line.startswith("---") or line.startswith("+++"):
            continue
        if line.startswith("\\"):
            # "\ No newline at end of file"
            continue
        if line.startswith("+"):
            kept.append(line[1:])
        elif line.startswith(" "):
            kept.append(line[1:])
        elif line.startswith("-"):
            continue
        else:
            kept.append(line)
    return "\n".join(kept) + ("\n" if kept else "")


def _doc_text(row: dict[str, Any], spec: TaskSpec) -> str:
    """文档正文：``text``；``title`` 非空且是代码文件时首行加注释。

    ``.sql`` / ``.txt`` **不加**注释（spec 要求不污染这两类文件）；``diff_postimage`` 的任务
    先做 diff → 后像代码规整。
    """
    text = row.get("text")
    body = text if isinstance(text, str) else ("" if text is None else str(text))
    if spec.diff_postimage:
        body = diff_postimage(body)
    title = row.get("title")
    title_text = title.strip() if isinstance(title, str) else ""
    if title_text and spec.code_like and spec.doc_ext in COMMENT_PREFIX:
        prefix = COMMENT_PREFIX[spec.doc_ext]
        return f"{prefix} {title_text}\n{body}"
    return body


def materialize_docs(
    spec: TaskSpec,
    rows: Sequence[dict[str, Any]],
    destination: Path | str,
    force: bool = False,
) -> tuple[dict[str, str], int]:
    """把池内文档逐个写成真实文件，返回 ``(docmap, 写出的文件数)``。

    ``docmap`` = ``{相对 POSIX 路径: doc_id}``，落盘到 ``<destination>/docmap.json``。
    """
    dest = Path(destination)
    docs_dir = dest / DOCS_SUBDIR
    docs_dir.mkdir(parents=True, exist_ok=True)
    docmap_path = dest / DOCMAP_NAME

    if not force and docmap_path.is_file():
        try:
            existing = json.loads(docmap_path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            existing = None
        if isinstance(existing, dict) and existing:
            return {str(k): str(v) for k, v in existing.items()}, len(existing)

    docmap: "OrderedDict[str, str]" = OrderedDict()
    written = 0
    for row in rows:
        doc_id = _row_doc_id(row)
        if not doc_id or doc_id in docmap.values():
            continue
        filename = safe_doc_filename(doc_id, spec.doc_ext)
        relative = f"{DOCS_SUBDIR}/{filename}"
        target = dest.joinpath(*relative.split("/"))
        target.parent.mkdir(parents=True, exist_ok=True)
        # 二进制写入，避免任何换行转换改变内容
        target.write_bytes(_doc_text(row, spec).encode("utf-8"))
        docmap[relative] = doc_id
        written += 1

    _atomic_write_text(docmap_path, json.dumps(docmap, ensure_ascii=False, indent=2))
    return dict(docmap), written


def git_init(directory: Path | str) -> tuple[bool, str]:
    """在语料目录执行 ``git init``（``codeseek init`` 的硬前置）。"""
    dest = Path(directory)
    dest.mkdir(parents=True, exist_ok=True)
    if (dest / ".git").exists():
        return True, "已存在 .git，跳过 git init"
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
        return False, f"git init 失败：{type(exc).__name__}: {exc}"
    if completed.returncode != 0:
        detail = (completed.stderr or completed.stdout or "").strip()[:300]
        return False, f"git init 返回 {completed.returncode}：{detail}"
    return True, "git init 完成"


# ---------------------------------------------------------------------------
# Task
# ---------------------------------------------------------------------------


@dataclass
class Task:
    """一道 MTEB(Code) 检索题（= 某任务的某条 query）。

    ``qrels`` 是**限定在池内**的 ``{doc_id: score}``：池化后 gold doc 若缺失，它不可能被检索到，
    与其在评分时当成漏检（不公平），不如从 qrels 里去掉并在 ``meta.json`` 里记录告警。
    """

    task_id: str
    task: str
    hf_dataset: str
    query_id: str
    query_text: str
    gold_doc_ids: list[str]
    corpus_dir: str
    docmap_path: str
    n_pool_docs: int
    qrels: dict[str, float] = field(default_factory=dict)
    qrels_split: str = ""
    language: str = ""

    def to_dict(self) -> dict[str, Any]:
        return asdict(self)

    @classmethod
    def from_dict(cls, payload: dict[str, Any]) -> "Task":
        known = {f for f in cls.__dataclass_fields__}  # type: ignore[attr-defined]
        return cls(**{k: v for k, v in payload.items() if k in known})


def write_tasks(tasks: Iterable[Task], path: Path | str) -> Path:
    """写 ``tasks.jsonl``（每题一行）。"""
    target = Path(path)
    target.parent.mkdir(parents=True, exist_ok=True)
    with open(target, "w", encoding="utf-8") as handle:
        for task in tasks:
            handle.write(json.dumps(task.to_dict(), ensure_ascii=False) + "\n")
    return target


def load_tasks(path: Path | str) -> list[Task]:
    """读 ``tasks.jsonl``。"""
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
    """写 ``<workdir>/meta.json``。"""
    target = Path(workdir) / WORKDIR_META
    _atomic_write_text(target, json.dumps(meta, ensure_ascii=False, indent=2))
    return target


def read_workdir_meta(workdir: Path | str) -> dict[str, Any]:
    """读 ``<workdir>/meta.json``；不存在/损坏时返回空 dict（不抛异常）。"""
    target = Path(workdir) / WORKDIR_META
    if not target.is_file():
        return {}
    try:
        payload = json.loads(target.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return {}
    return payload if isinstance(payload, dict) else {}


# ---------------------------------------------------------------------------
# prepare 主流程
# ---------------------------------------------------------------------------


@dataclass
class PrepareOptions:
    """``prepare`` 的抽样口径。"""

    workdir: Path
    corpus_size: int = 300
    limit_queries: int = 30
    seed: int = 42
    force: bool = False
    quiet: bool = False


@dataclass
class PreparedTask:
    """单个任务的 prepare 产物：题目列表 + 写进 ``meta.json`` 的摘要。"""

    spec: TaskSpec
    tasks: list[Task]
    meta: dict[str, Any]


def _query_text(row: dict[str, Any]) -> str:
    """query 文本；``SyntheticText2SQL`` 要把 SQL schema（``context``）拼上去。"""
    text = row.get("text")
    base = text if isinstance(text, str) else ("" if text is None else str(text))
    context = row.get("context")
    schema = context.strip() if isinstance(context, str) else ""
    if schema:
        return f"{base}\n\nSchema:\n{schema}"
    return base


def prepare_task(spec: TaskSpec, client: DatasetClient, options: PrepareOptions) -> PreparedTask:
    """为单个任务完成「qrels → 抽 query → 池化 corpus → 物化 → 返回题目与 meta 摘要」。

    :return: :class:`PreparedTask`（``tasks`` 供写 ``tasks.jsonl``，``meta`` 供写 ``meta.json``）。
    """
    warnings: list[str] = []
    if spec.excluded_reason:
        raise DatasetError(
            f"{spec.name}: 注册表里该任务已被**实测剔除**（excluded_reason={spec.excluded_reason!r}）。"
            "剔除是方法学决定，不允许临时选中，否则批次之间的分数不可比对。"
        )
    if spec.unverified:
        raise DatasetError(
            f"{spec.name}: 注册表里该任务的 config/split 未经验证（unverified=True），"
            "请先手工核对 /splits 再补齐 evals/mtebcode/mtebcode_eval/data.py 的 TASKS"
        )

    layout = resolve_layout(client, spec)
    qrels_split = resolve_qrels_split(client, spec, layout)

    corpus_rows_total = client.num_rows(spec.hf_dataset, layout.corpus_config, layout.corpus_split)
    queries_rows_total = client.num_rows(spec.hf_dataset, layout.queries_config, layout.queries_split)

    qrels_all, qrels_read = fetch_qrels(client, spec.hf_dataset, layout.qrels_config, qrels_split)
    if not qrels_all:
        raise DatasetError(
            f"{spec.name}: {spec.hf_dataset} 的 qrels（{layout.qrels_config}/{qrels_split}）没有 score>0 的记录"
        )

    # 每任务配额优先（注册表 self-describing），否则用全局 --limit-queries
    effective_limit = spec.query_quota if spec.query_quota is not None else options.limit_queries
    candidates = sample_query_ids(list(qrels_all), effective_limit, options.seed, spec.name)
    if spec.shard_index is not None and spec.shard_total is not None:
        # 确定性分片：候选集（分片任务的 query_quota=0，故这里是该语言全部 query）排序后
        # 连续均分成 shard_total 片，本任务只取第 shard_index 片 —— 100 片并集 = 全部 query。
        query_ids = shard_query_ids(candidates, spec.shard_index, spec.shard_total)
    else:
        query_ids = candidates
    if not query_ids:
        raise DatasetError(f"{spec.name}: 抽样后没有可用 query")

    query_rows, query_stats = fetch_rows_by_ids(
        client, spec.hf_dataset, layout.queries_config, layout.queries_split, query_ids, queries_rows_total
    )
    missing_queries = [qid for qid in query_ids if qid not in query_rows]
    if missing_queries:
        warnings.append(
            f"{len(missing_queries)} 条 query 在 {layout.queries_config}/{layout.queries_split} 里找不到"
            f"（例如 {missing_queries[0]}），已跳过"
        )
        query_ids = [qid for qid in query_ids if qid in query_rows]

    # ---- gold doc ----
    gold_by_query: dict[str, list[str]] = {}
    for query_id in query_ids:
        graded = qrels_all.get(query_id, {})
        gold_by_query[query_id] = [
            doc_id for doc_id, _score in sorted(graded.items(), key=lambda pair: (-pair[1], pair[0]))
        ]
    gold_ids_ordered: list[str] = []
    seen_gold: set[str] = set()
    for query_id in query_ids:
        for doc_id in gold_by_query[query_id]:
            if doc_id not in seen_gold:
                seen_gold.add(doc_id)
                gold_ids_ordered.append(doc_id)

    truncated = False
    distractor_rows: list[dict[str, Any]] = []
    distractor_stats = FetchStats(mode="skip")
    pool_rows: list[dict[str, Any]] = []
    seen_docs: set[str] = set()

    if spec.full_corpus:
        # **全量物化**：同 corpus_key 的所有分片必须看到逐字节一致的 docs/ 与 docmap.json，
        # 否则共享索引里 A 分片的 gold 会指向 B 分片写的文件。因此这里按 corpus 的**原始顺序**
        # 物化全部行：不截断 gold、不抽随机干扰文档（--corpus-size 对该任务不生效）。
        gold_stats = FetchStats(mode="full")
        for page in iter_rows(
            client, spec.hf_dataset, layout.corpus_config, layout.corpus_split, corpus_rows_total
        ):
            gold_stats.pages += 1
            gold_stats.rows_scanned += len(page)
            for row in page:
                doc_id = _row_doc_id(row)
                if not doc_id or doc_id in seen_docs:
                    continue
                seen_docs.add(doc_id)
                pool_rows.append(row)
        missing_gold = [doc_id for doc_id in gold_ids_ordered if doc_id not in seen_docs]
        if missing_gold:
            warnings.append(
                f"{len(missing_gold)} 个 gold doc 在 corpus 里找不到"
                f"（例如 {missing_gold[0]}），该 query 的 qrels 会被裁剪"
            )
    else:
        if len(gold_ids_ordered) > options.corpus_size:
            truncated = True
            warnings.append(
                f"gold doc 数 {len(gold_ids_ordered)} 超过 --corpus-size {options.corpus_size}，"
                f"已截断到 {options.corpus_size}（部分 query 的 gold 会缺失，指标偏低）"
            )
            gold_ids_ordered = gold_ids_ordered[: options.corpus_size]
            seen_gold = set(gold_ids_ordered)

        need_distractors = max(0, options.corpus_size - len(gold_ids_ordered))
        distractor_rows, distractor_stats = sample_rows(
            client,
            spec.hf_dataset,
            layout.corpus_config,
            layout.corpus_split,
            corpus_rows_total,
            need_distractors,
            options.seed,
            exclude=set(seen_gold),
        )

        gold_rows, gold_stats = fetch_rows_by_ids(
            client, spec.hf_dataset, layout.corpus_config, layout.corpus_split, gold_ids_ordered, corpus_rows_total
        )
        if gold_stats.missing:
            warnings.append(
                f"{len(gold_stats.missing)} 个 gold doc 在 corpus 里找不到"
                f"（例如 {gold_stats.missing[0]}），该 query 的 qrels 会被裁剪"
            )

        for row in list(gold_rows.values()) + distractor_rows:
            doc_id = _row_doc_id(row)
            if not doc_id or doc_id in seen_docs:
                continue
            seen_docs.add(doc_id)
            pool_rows.append(row)

    destination = corpus_dir(options.workdir, spec)
    docmap, written = materialize_docs(spec, pool_rows, destination, force=options.force)
    pool_ids = set(docmap.values())
    git_ok, git_detail = git_init(destination)

    # ---- 组装 Task ----
    tasks: list[Task] = []
    for index, query_id in enumerate(query_ids):
        graded = {doc_id: score for doc_id, score in qrels_all.get(query_id, {}).items() if doc_id in pool_ids}
        golds = [doc_id for doc_id in gold_by_query[query_id] if doc_id in pool_ids]
        if not graded:
            warnings.append(f"query {query_id} 的 gold doc 全部不在池内，该题会被跳过")
        tasks.append(
            Task(
                task_id=f"{spec.slug}__q{index:04d}",
                task=spec.name,
                hf_dataset=spec.hf_dataset,
                query_id=query_id,
                query_text=_query_text(query_rows[query_id]),
                gold_doc_ids=golds,
                corpus_dir=str(destination),
                docmap_path=str(destination / DOCMAP_NAME),
                n_pool_docs=len(pool_ids),
                qrels={doc_id: float(score) for doc_id, score in graded.items()},
                qrels_split=qrels_split,
                language=spec.language,
            )
        )

    if not git_ok:
        warnings.append(f"git init 失败（codeseek init 会因 `No project found` 失败）：{git_detail}")

    # query 字符长度分布（写进 meta.json）：第 2 批的整个方法学是「短 query」，
    # 因此把实测长度钉在产物里，报告不必再去猜。
    query_lengths = sorted(len(task.query_text) for task in tasks)

    def _length_pct(percentile: float) -> int:
        if not query_lengths:
            return 0
        index = min(len(query_lengths) - 1, int(round(percentile * (len(query_lengths) - 1))))
        return query_lengths[index]

    query_length_stats = {
        "n": len(query_lengths),
        "min": query_lengths[0] if query_lengths else 0,
        "median": _length_pct(0.5),
        "p90": _length_pct(0.9),
        "max": query_lengths[-1] if query_lengths else 0,
        "mean": round(sum(query_lengths) / len(query_lengths), 1) if query_lengths else 0.0,
        "gt_200": sum(1 for value in query_lengths if value > 200),
    }

    if not options.quiet:
        shard_note = (
            f" [shard {spec.shard_index + 1:02d}/{spec.shard_total}]"
            if spec.shard_index is not None and spec.shard_total is not None
            else ""
        )
        _log(
            f"[prepare] {spec.name}{shard_note}: qrels={layout.qrels_config}/{qrels_split} "
            f"({len(qrels_all)} queries, {qrels_read} rows) → 抽 {len(tasks)} 题；"
            f"池 {len(pool_ids)} doc（gold {len(gold_ids_ordered)} + 干扰 {len(distractor_rows)}）；"
            f"corpus 扫描 {gold_stats.mode}/{gold_stats.pages}p + 抽样 {distractor_stats.pages}p"
        )

    return PreparedTask(
        spec=spec,
        tasks=tasks,
        meta={
            "task": spec.name,
            "task_slug": spec.slug,
            "hf_dataset": spec.hf_dataset,
            "corpus_config": layout.corpus_config,
            "corpus_split": layout.corpus_split,
            "queries_config": layout.queries_config,
            "queries_split": layout.queries_split,
            "qrels_config": layout.qrels_config,
            "qrels_split": qrels_split,
            "doc_ext": spec.doc_ext,
            "language": spec.language,
            # 共享语料 / 分片口径必须显式记录：同一 corpus_key 的所有分片共用一次 init
            "corpus_key": spec.corpus_key,
            "full_corpus": bool(spec.full_corpus),
            "shard": {"index": spec.shard_index, "total": spec.shard_total},
            # 物化口径必须显式记录：diff_postimage 会改变写盘内容（语料可索引性的必要条件）
            "materialization": {
                "doc_ext": spec.doc_ext,
                "diff_postimage": bool(spec.diff_postimage),
                "title_comment": bool(spec.code_like and spec.doc_ext in COMMENT_PREFIX),
            },
            "query_quota": effective_limit,
            "query_length_chars": query_length_stats,
            "num_corpus_rows": corpus_rows_total,
            "num_queries_rows": queries_rows_total,
            "num_qrels_rows": qrels_read,
            "num_queries_sampled": len(tasks),
            "num_pool_docs": len(pool_ids),
            "num_gold_docs": len(gold_ids_ordered),
            "num_distractor_docs": len(distractor_rows),
            # full_corpus 任务不看 --corpus-size：目标池大小就是实际物化的全量文档数
            "n_pool_docs_target": len(pool_ids) if spec.full_corpus else options.corpus_size,
            "gold_truncated": truncated,
            "docs_written": written,
            "corpus_dir": str(destination),
            "docmap_path": str(destination / DOCMAP_NAME),
            "git": {"ok": git_ok, "detail": git_detail},
            "fetch": {
                "corpus": gold_stats.to_dict(),
                "distractors": distractor_stats.to_dict(),
                "queries": query_stats.to_dict(),
            },
            "warnings": warnings,
        },
    )


def sampling_fingerprint(tasks: Sequence[Task], seed: int, corpus_size: int, limit_queries: int) -> str:
    """抽样指纹：把 task_id / query_id / gold 集合 / 口径参数一起哈希，便于比对两次 prepare。"""
    payload = json.dumps(
        {
            "seed": seed,
            "corpus_size": corpus_size,
            "limit_queries": limit_queries,
            "tasks": [
                {"task_id": task.task_id, "query_id": task.query_id, "gold": sorted(task.gold_doc_ids)}
                for task in tasks
            ],
        },
        ensure_ascii=False,
        sort_keys=True,
    )
    return hashlib.sha256(payload.encode("utf-8")).hexdigest()[:16]


def benchmark_meta_header(source: str, command: str) -> dict[str, Any]:
    """报告 / meta.json 的公共头部字段。"""
    return {
        "generated_at": _utc_now(),
        "benchmark": BENCHMARK,
        "source": source,
        "command": command,
    }
