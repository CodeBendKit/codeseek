"""评测编排：串行 init → 并发 search → 断点续跑 → 评分 → 出报告。

设计要点（与 ``evals/repoqa/repoqa_eval/runner.py`` 完全同构）：

* ``codeseek init`` 是重活（全量解析 + 建索引 + 可能触发 embedding），**按语料目录串行**
  执行，同一目录只做一次；结果缓存在内存里。
* ``codeseek search`` 是轻查询，用 ``ThreadPoolExecutor`` 并发（默认 4）。
* 每题的检索结果**增量落盘**到 ``<workdir>/raw/<task_id>.json``（``.json.tmp`` → 原子 rename）：
  中断后重跑会直接复用，``--force`` 才重跑。这样长跑可以分段推进。
* 单语料 init 失败 / 单题 search 超时**都不中断全局**：记为失败并继续，最后在报告里汇总。
* raw 里只保留诊断字段（``code_block`` 只留长度），既能让 ``score`` 子命令离线重算指标，
  又不会把 raw 目录撑爆。
"""

from __future__ import annotations

import json
import sys
import threading
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Mapping, Sequence

from . import adapter, report as report_module, scorer
from .data import BENCHMARK, RAW_DIR, Task

__all__ = ["RunSummary", "run", "score_only"]

#: raw 里保留的结果字段（``code_block`` 只留长度，避免 raw 过大）。
_KEEP_FIELDS: tuple[str, ...] = (
    "file_path",
    "path",
    "name",
    "symbol_name",
    "symbol_type",
    "language",
    "line_start",
    "line_end",
    "score",
)


def _stderr(message: str) -> None:
    print(message, file=sys.stderr, flush=True)


def _now_iso() -> str:
    return datetime.now(tz=timezone.utc).isoformat(timespec="seconds")


@dataclass
class RunSummary:
    """``run`` / ``score_only`` 的返回值（供 cli 打印尾部摘要）。"""

    total_tasks: int
    executed: int
    skipped: int
    init_failures: int
    mode_counts: dict[str, int]
    report: dict[str, Any]
    report_json_path: str
    report_md_path: str
    warnings: list[str] = field(default_factory=list)

    @property
    def mean_ndcg(self) -> float | None:
        value = (self.report.get("aggregate") or {}).get("mean_ndcg_at_10")
        return float(value) if value is not None else None


def _raw_path(workdir: Path, task_id: str) -> Path:
    return workdir / RAW_DIR / f"{task_id}.json"


def _read_raw(path: Path) -> dict[str, Any] | None:
    if not path.is_file():
        return None
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return None
    return payload if isinstance(payload, dict) else None


def _write_raw(path: Path, payload: Mapping[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(".json.tmp")
    tmp.write_text(json.dumps(dict(payload), ensure_ascii=False, indent=2), encoding="utf-8")
    tmp.replace(path)


def _trim_results(results: Sequence[Mapping[str, Any]]) -> list[dict[str, Any]]:
    """只保留诊断 / 反查需要的字段；``code_block`` 用长度代替。"""
    trimmed: list[dict[str, Any]] = []
    for item in results:
        if not isinstance(item, dict):
            continue
        row: dict[str, Any] = {key: item.get(key) for key in _KEEP_FIELDS if key in item}
        block = item.get("code_block")
        if isinstance(block, str):
            row["code_block_chars"] = len(block)
        trimmed.append(row)
    return trimmed


def _corpus_key(task: Task) -> str:
    return task.corpus_dir or ""


def _docs_total(tasks: Sequence[Task]) -> int:
    """不同语料目录的池大小之和（同一个 task 的多条 query 共享同一语料）。"""
    pools: dict[str, int] = {}
    for task in tasks:
        key = _corpus_key(task)
        pools[key] = max(pools.get(key, 0), int(task.n_pool_docs or 0))
    return sum(pools.values())


def _embedding_block(configured: bool, note: str) -> dict[str, Any]:
    """把 ``(是否配置, 说明)`` 整理成报告 meta 里的 ``embedding`` 字段。"""
    model = ""
    marker = "model="
    if marker in note:
        tail = note.split(marker, 1)[1]
        model = tail.split(",")[0].split(")")[0].strip()
    return {"configured": bool(configured), "model": model or "<unknown>", "note": note}


def run(
    tasks: Sequence[Task],
    workdir: Path | str,
    binary: Path,
    limit: int = 20,
    k: int = scorer.DEFAULT_K,
    concurrency: int = 4,
    timeout: float = 180.0,
    init_timeout: float = 1800.0,
    force: bool = False,
    embedding_configured: bool = False,
    embedding_note: str = "",
    run_meta: Mapping[str, Any] | None = None,
    progress: bool = True,
) -> RunSummary:
    """执行一轮完整评测：init → search → score → report。

    :param tasks: 已抽好的题目（可被 ``--limit-queries`` 截断）。
    :param workdir: 所有产物的根目录（默认 ``/tmp/mtebcode-eval``）。
    :param binary: codeseek 二进制路径。
    :param limit: ``codeseek search --limit`` 的 K（同时是 ``recall@k`` 的 k）。
    :param k: 主指标 nDCG@k 的 k。
    :param force: 为 True 时忽略已有 raw 结果，强制重跑检索。
    """
    workdir_path = Path(workdir).expanduser()
    (workdir_path / RAW_DIR).mkdir(parents=True, exist_ok=True)

    groups: "dict[str, list[Task]]" = {}
    for task in tasks:
        groups.setdefault(_corpus_key(task), []).append(task)

    started_all = time.monotonic()

    # ---------- 1) 串行 init（每语料一次） ----------
    init_ok: dict[str, bool] = {}
    init_detail: dict[str, str] = {}
    if progress:
        _stderr(f"[runner] 共 {len(tasks)} 题 / {len(groups)} 个语料目录，开始建索引 ...")

    for position, (corpus, group) in enumerate(groups.items(), start=1):
        label = Path(corpus).name if corpus else "<empty>"
        if not corpus or not Path(corpus).is_dir():
            init_ok[corpus] = False
            init_detail[corpus] = f"语料目录不存在：{corpus}（先运行 `prepare`）"
            if progress:
                _stderr(f"[init {position}/{len(groups)}] {label} … SKIP（目录不存在）")
            continue

        if not force and all(_raw_path(workdir_path, task.task_id).is_file() for task in group):
            init_ok[corpus] = True
            init_detail[corpus] = "已有全部题目的 raw 结果，跳过 init"
            if progress:
                _stderr(f"[init {position}/{len(groups)}] {label} … 复用已有结果，跳过")
            continue

        if progress:
            _stderr(f"[init {position}/{len(groups)}] {label} … codeseek init 执行中")
        started = time.monotonic()
        ok, detail = adapter.init_repo(binary, Path(corpus), timeout=init_timeout)
        elapsed = time.monotonic() - started
        init_ok[corpus] = ok
        init_detail[corpus] = detail
        if progress:
            status = "OK" if ok else "FAIL"
            _stderr(f"[init {position}/{len(groups)}] {label} … {status} ({elapsed:.1f}s)")
            if not ok and detail:
                for line in detail.splitlines()[-3:]:
                    _stderr(f"        {line}")

    # ---------- 2) 并发 search ----------
    total = len(tasks)
    counter = {"done": 0, "executed": 0, "skipped": 0}
    lock = threading.Lock()

    def _search_one(task: Task) -> dict[str, Any]:
        corpus = _corpus_key(task)
        raw_file = _raw_path(workdir_path, task.task_id)

        cached = None if force else _read_raw(raw_file)
        if cached is not None:
            with lock:
                counter["skipped"] += 1
            return cached

        if not init_ok.get(corpus, False):
            payload = {
                "task_id": task.task_id,
                "task": task.task,
                "query_id": task.query_id,
                "query": task.query_text,
                "corpus_dir": corpus,
                "mode": adapter.MODE_UNKNOWN,
                "results_count": 0,
                "results": [],
                "error": f"init_failed: {init_detail.get(corpus, 'unknown')}",
                "elapsed_s": 0.0,
                "retrieved_at": _now_iso(),
            }
            _write_raw(raw_file, payload)
            with lock:
                counter["executed"] += 1
            return payload

        outcome = adapter.search(binary, Path(corpus), task.query_text, limit=limit, timeout=timeout)
        payload = {
            "task_id": task.task_id,
            "task": task.task,
            "query_id": task.query_id,
            "query": task.query_text,
            "corpus_dir": corpus,
            "mode": outcome.mode,
            "results_count": len(outcome.results),
            "results": _trim_results(outcome.results),
            "error": outcome.error,
            "elapsed_s": round(outcome.elapsed_s, 3),
            "retrieved_at": _now_iso(),
        }
        _write_raw(raw_file, payload)
        with lock:
            counter["executed"] += 1
        return payload

    def process(task: Task) -> dict[str, Any]:
        """单题兜底包装：任何异常都转成一条失败记录，不中断全局。"""
        try:
            payload = _search_one(task)
        except Exception as exc:  # noqa: BLE001 - 故意兜住所有异常，保证长跑能跑完
            payload = {
                "task_id": task.task_id,
                "task": task.task,
                "query_id": task.query_id,
                "query": task.query_text,
                "corpus_dir": _corpus_key(task),
                "mode": adapter.MODE_UNKNOWN,
                "results_count": 0,
                "results": [],
                "error": f"exception: {type(exc).__name__}: {exc}",
                "elapsed_s": 0.0,
                "retrieved_at": _now_iso(),
            }
            try:
                _write_raw(_raw_path(workdir_path, task.task_id), payload)
            except OSError:
                pass

        with lock:
            counter["done"] += 1
            index = counter["done"]
        if progress:
            print(
                f"[{index}/{total}] {task.task_id} task={task.task} mode={payload.get('mode')} "
                f"returned={payload.get('results_count')} {float(payload.get('elapsed_s') or 0):.1f}s",
                flush=True,
            )
        return payload

    if concurrency <= 1:
        for task in tasks:
            process(task)
    else:
        with ThreadPoolExecutor(max_workers=int(concurrency)) as pool:
            futures = [pool.submit(process, task) for task in tasks]
            for future in as_completed(futures):
                future.result()

    # ---------- 3) 评分（从磁盘重新读 raw，保证与 `score` 子命令完全一致） ----------
    raws = {task.task_id: (_read_raw(_raw_path(workdir_path, task.task_id)) or {}) for task in tasks}
    scoring = scorer.score_tasks(tasks, raws, k=k, search_limit=limit)

    mode_counts: dict[str, int] = {}
    for payload in raws.values():
        mode = str(payload.get("mode") or adapter.MODE_UNKNOWN)
        mode_counts[mode] = mode_counts.get(mode, 0) + 1

    elapsed_total = time.monotonic() - started_all
    meta = {
        "generated_at": _now_iso(),
        "benchmark": BENCHMARK,
        "command": "run",
        "tasks": sorted({task.task for task in tasks}),
        "dataset_splits": {task.task: task.qrels_split for task in tasks},
        "n_queries": len(tasks),
        "n_queries_all": len(tasks),
        "n_docs_total": _docs_total(tasks),
        "k": int(k),
        "search_limit": int(limit),
        "concurrency": int(concurrency),
        "search_timeout_s": float(timeout),
        "init_timeout_s": float(init_timeout),
        "elapsed_s": round(elapsed_total, 1),
        "mode_counts": mode_counts,
        "embedding": _embedding_block(embedding_configured, embedding_note),
        "embedding_note": embedding_note,
        "init_failures": [
            {"corpus_dir": corpus, "detail": detail}
            for corpus, ok in init_ok.items()
            if not ok
        ],
        **dict(run_meta or {}),
    }
    report = report_module.build_report(scoring, raws, meta)
    json_path, md_path = report_module.write_reports(workdir_path, report)

    if progress:
        mean = report["aggregate"]["mean_ndcg_at_10"]
        shown = "n/a" if mean is None else f"{mean:.4f}"
        _stderr(
            f"[runner] 完成：{total} 题 mean nDCG@{k} = {shown}"
            f"（executed={counter['executed']} skipped={counter['skipped']} "
            f"init_failures={sum(1 for ok in init_ok.values() if not ok)}）"
        )

    return RunSummary(
        total_tasks=total,
        executed=counter["executed"],
        skipped=counter["skipped"],
        init_failures=sum(1 for ok in init_ok.values() if not ok),
        mode_counts=mode_counts,
        report=report,
        report_json_path=str(json_path),
        report_md_path=str(md_path),
        warnings=list(report.get("warnings") or []),
    )


def score_only(
    tasks: Sequence[Task],
    workdir: Path | str,
    k: int = scorer.DEFAULT_K,
    search_limit: int = 20,
    extra_meta: Mapping[str, Any] | None = None,
) -> RunSummary:
    """只用已有 raw 结果重新评分（``score`` 子命令；不重跑检索）。

    与 :func:`run` 共用同一套聚合 / 出报告代码，保证两种入口产出一致。
    """
    workdir_path = Path(workdir).expanduser()
    missing = [task.task_id for task in tasks if not _raw_path(workdir_path, task.task_id).is_file()]
    if missing:
        raise FileNotFoundError(
            f"{len(missing)} 道题缺少 raw 结果（例如 {missing[0]}）；先运行 `run` 或缩小 --tasks/--limit-queries"
        )

    raws = {task.task_id: (_read_raw(_raw_path(workdir_path, task.task_id)) or {}) for task in tasks}
    scoring = scorer.score_tasks(tasks, raws, k=k, search_limit=search_limit)

    mode_counts: dict[str, int] = {}
    for payload in raws.values():
        mode = str(payload.get("mode") or adapter.MODE_UNKNOWN)
        mode_counts[mode] = mode_counts.get(mode, 0) + 1

    extra = dict(extra_meta or {})
    meta = {
        "generated_at": _now_iso(),
        "benchmark": BENCHMARK,
        "command": "score",
        "rescore_only": True,
        "tasks": sorted({task.task for task in tasks}),
        "dataset_splits": {task.task: task.qrels_split for task in tasks},
        "n_queries": len(tasks),
        "n_docs_total": _docs_total(tasks),
        "k": int(k),
        "search_limit": int(search_limit),
        "mode_counts": mode_counts,
        "embedding": _embedding_block(
            bool(extra.get("embedding_configured", False)), str(extra.get("embedding_note") or "not checked")
        ),
        "embedding_note": extra.get("embedding_note") or "not checked",
        **extra,
    }
    report = report_module.build_report(scoring, raws, meta)
    json_path, md_path = report_module.write_reports(workdir_path, report)

    return RunSummary(
        total_tasks=len(tasks),
        executed=0,
        skipped=len(tasks),
        init_failures=0,
        mode_counts=mode_counts,
        report=report,
        report_json_path=str(json_path),
        report_md_path=str(md_path),
        warnings=list(report.get("warnings") or []),
    )
