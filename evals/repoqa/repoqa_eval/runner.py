"""评测编排：串行 init → 并发 search → 断点续跑 → 判分 → 出报告。

设计要点：

* ``codeseek init`` 是重活（全量解析 + 建索引 + 可能触发 embedding），**按 repo 串行**执行，
  同 repo 只做一次；结果缓存在内存里。
* ``codeseek search`` 是轻查询，用 ``ThreadPoolExecutor`` 并发（默认 4）。
* 每题的检索结果**增量落盘**到 ``<workdir>/raw/<task_id>.json``：中断后重跑会直接复用，
  ``--force`` 才重跑。这样 100 题的长跑可以分段推进。
* 单 repo init 失败 / 单题 search 超时**都不中断全局**：记为 fail 并继续，最后在报告里汇总。
"""

from __future__ import annotations

import json
import sys
import threading
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Sequence

from . import adapter, report as report_module, scorer
from .data import RAW_DIR, Task, repo_dir

__all__ = ["RunSummary", "run"]


def _stderr(message: str) -> None:
    print(message, file=sys.stderr, flush=True)


@dataclass
class RunSummary:
    """``run`` 的返回值（供 cli 打印尾部摘要）。"""

    total_tasks: int
    executed: int
    skipped: int
    init_failures: int
    mode_counts: dict[str, int]
    report: dict[str, Any]
    report_json_path: str
    report_md_path: str


def _repo_key(task: Task) -> tuple[str, int, str]:
    return (task.language, task.repo_index, task.repo)


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


def _write_raw(path: Path, payload: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(".json.tmp")
    tmp.write_text(json.dumps(payload, ensure_ascii=False, indent=2), encoding="utf-8")
    tmp.replace(path)


def _results_meta(results: Sequence[dict[str, Any]]) -> list[dict[str, Any]]:
    """只保留诊断用的少量字段，避免 raw 文件过大。"""
    meta: list[dict[str, Any]] = []
    for item in results:
        meta.append(
            {
                "symbol": item.get("symbol_name") or item.get("name"),
                "file_path": item.get("file_path"),
                "line_start": item.get("line_start"),
                "line_end": item.get("line_end"),
                "score": item.get("score"),
            }
        )
    return meta


def run(
    tasks: Sequence[Task],
    dataset: dict[str, list[dict[str, Any]]],
    workdir: Path | str,
    binary: Path,
    limit: int = 5,
    threshold: float = scorer.DEFAULT_THRESHOLD,
    concurrency: int = 4,
    timeout: float = 180.0,
    init_timeout: float = 1800.0,
    force: bool = False,
    embedding_configured: bool = False,
    embedding_note: str = "",
    run_meta: dict[str, Any] | None = None,
    progress: bool = True,
) -> RunSummary:
    """执行一轮完整评测，返回评分结果并写出 ``report.json`` / ``report.md``。

    :param tasks: 已抽好的题目（可被 ``--limit-questions`` 截断）。
    :param dataset: 原始数据集（判分需要 ``content`` / ``needles``）。
    :param workdir: 所有产物根目录（默认 ``/tmp/repoqa-eval``）。
    :param binary: codeseek 二进制路径。
    :param limit: ``codeseek search --limit`` 的 K。
    :param force: 为 True 时忽略已有 raw 结果，强制重跑检索。
    """
    workdir_path = Path(workdir)
    (workdir_path / RAW_DIR).mkdir(parents=True, exist_ok=True)

    grouped: dict[tuple[str, int, str], list[Task]] = {}
    for task in tasks:
        grouped.setdefault(_repo_key(task), []).append(task)

    started_all = time.monotonic()

    # ---------- 1) 串行 init（每 repo 一次） ----------
    init_ok: dict[tuple[str, int, str], bool] = {}
    init_detail: dict[tuple[str, int, str], str] = {}
    if progress:
        _stderr(f"[runner] 共 {len(tasks)} 题 / {len(grouped)} 个仓库，开始建索引 ...")

    for position, (key, repo_tasks) in enumerate(grouped.items(), start=1):
        language, _repo_index, repo = key
        directory = repo_dir(workdir_path, language, repo)
        if not directory.is_dir():
            init_ok[key] = False
            init_detail[key] = f"仓库快照不存在：{directory}（先运行 `prepare`）"
            if progress:
                _stderr(f"[init {position}/{len(grouped)}] {language} {repo} … SKIP（快照不存在）")
            continue

        if not force:
            # 断点续跑：若该 repo 所有题都已有 raw 结果，就没必要重新 init（省掉最贵的一步）。
            if all(_raw_path(workdir_path, task.id).is_file() for task in repo_tasks):
                init_ok[key] = True
                init_detail[key] = "已有全部题目的 raw 结果，跳过 init"
                if progress:
                    _stderr(f"[init {position}/{len(grouped)}] {language} {repo} … 复用已有结果，跳过")
                continue

        if progress:
            _stderr(f"[init {position}/{len(grouped)}] {language} {repo} … codeseek init 执行中")
        started = time.monotonic()
        ok, detail = adapter.init_repo(binary, directory, timeout=init_timeout)
        elapsed = time.monotonic() - started
        init_ok[key] = ok
        init_detail[key] = detail
        if progress:
            status = "OK" if ok else "FAIL"
            _stderr(f"[init {position}/{len(grouped)}] {language} {repo} … {status} ({elapsed:.1f}s)")
            if not ok and detail:
                for line in detail.splitlines()[-3:]:
                    _stderr(f"        {line}")

    # ---------- 2) 并发 search ----------
    total = len(tasks)
    counter = {"done": 0, "executed": 0, "skipped": 0}
    lock = threading.Lock()

    def _process_impl(task: Task) -> dict[str, Any]:
        """处理单题：必要时检索 → 落盘 raw → 立即判分（供进度行显示）。"""
        key = _repo_key(task)
        raw_file = _raw_path(workdir_path, task.id)
        repo_path = repo_dir(workdir_path, task.language, task.repo)

        cached = None if force else _read_raw(raw_file)
        if cached is not None:
            with lock:
                counter["skipped"] += 1
        elif not init_ok.get(key, False):
            cached = {
                "task_id": task.id,
                "language": task.language,
                "repo": task.repo,
                "needle_name": task.needle_name,
                "query": task.description,
                "mode": adapter.MODE_UNKNOWN,
                "results_count": 0,
                "outputs": [],
                "results_meta": [],
                "error": f"init_failed: {init_detail.get(key, 'unknown')}",
                "elapsed_s": 0.0,
                "retrieved_at": _now_iso(),
            }
            _write_raw(raw_file, cached)
            with lock:
                counter["executed"] += 1
        else:
            outcome = adapter.search(
                binary, repo_path, task.description, limit=limit, timeout=timeout
            )
            outputs = (
                adapter.build_outputs(outcome.results, repo_path, task.language)
                if outcome.results
                else []
            )
            cached = {
                "task_id": task.id,
                "language": task.language,
                "repo": task.repo,
                "needle_name": task.needle_name,
                "query": task.description,
                "mode": outcome.mode,
                "results_count": len(outcome.results),
                "outputs": outputs,
                "results_meta": _results_meta(outcome.results),
                "error": outcome.error,
                "elapsed_s": round(outcome.elapsed_s, 3),
                "retrieved_at": _now_iso(),
            }
            _write_raw(raw_file, cached)
            with lock:
                counter["executed"] += 1

        score = scorer.score_one(
            task,
            cached.get("outputs", []),
            dataset[task.language][task.repo_index],
            threshold=threshold,
        )

        with lock:
            counter["done"] += 1
            index = counter["done"]
        if progress:
            print(
                f"[{index}/{total}] {task.language} {task.repo} needle={task.needle_name} "
                f"mode={cached.get('mode')} pass={score['passed']} "
                f"sim={score['best_similarity']:.3f} {float(cached.get('elapsed_s') or 0):.1f}s",
                flush=True,
            )
        return {"task": task, "raw": cached, "score": score}

    def process(task: Task) -> dict[str, Any]:
        """``_process_impl`` 的**兜底包装**：单题任何异常都转成一条 fail 记录，不中断全局。"""
        try:
            return _process_impl(task)
        except Exception as exc:  # noqa: BLE001 - 故意兜住所有异常，保证 100 题能跑完
            failed_raw = {
                "task_id": task.id,
                "language": task.language,
                "repo": task.repo,
                "needle_name": task.needle_name,
                "query": task.description,
                "mode": adapter.MODE_UNKNOWN,
                "results_count": 0,
                "outputs": [],
                "results_meta": [],
                "error": f"exception: {type(exc).__name__}: {exc}",
                "elapsed_s": 0.0,
                "retrieved_at": _now_iso(),
            }
            try:
                _write_raw(_raw_path(workdir_path, task.id), failed_raw)
            except OSError:
                pass
            score = scorer.score_one(
                task,
                [],
                dataset[task.language][task.repo_index],
                threshold=threshold,
            )
            with lock:
                counter["done"] += 1
                index = counter["done"]
            if progress:
                print(
                    f"[{index}/{total}] {task.language} {task.repo} needle={task.needle_name} "
                    f"mode=unknown_empty pass=False sim=0.000 0.0s  (exception: {type(exc).__name__})",
                    flush=True,
                )
            return {"task": task, "raw": failed_raw, "score": score}

    results: list[dict[str, Any]] = []
    if concurrency <= 1:
        for task in tasks:
            results.append(process(task))
    else:
        with ThreadPoolExecutor(max_workers=concurrency) as pool:
            futures = [pool.submit(process, task) for task in tasks]
            for future in as_completed(futures):
                results.append(future.result())

    # ---------- 3) 判分（从 raw 重新聚合，保证与 `score` 子命令一致） ----------
    outputs_by_task_id = {
        task.id: (_read_raw(_raw_path(workdir_path, task.id)) or {}).get("outputs", [])
        for task in tasks
    }
    scoring = scorer.score_tasks(tasks, outputs_by_task_id, dataset, threshold=threshold)

    raws = {task.id: _read_raw(_raw_path(workdir_path, task.id)) or {} for task in tasks}
    mode_counts: dict[str, int] = {}
    for payload in raws.values():
        mode = payload.get("mode") or adapter.MODE_UNKNOWN
        mode_counts[mode] = mode_counts.get(mode, 0) + 1

    elapsed_total = time.monotonic() - started_all
    meta = {
        "generated_at": _now_iso(),
        "tasks": total,
        "repos": len(grouped),
        "threshold": threshold,
        "search_limit": limit,
        "concurrency": concurrency,
        "search_timeout_s": timeout,
        "init_timeout_s": init_timeout,
        "elapsed_s": round(elapsed_total, 1),
        "bleu_backend": scoring["bleu_backend"],
        "treesitter_available": scorer.treesitter_available(),
        "mode_counts": mode_counts,
        "embedding_configured": embedding_configured,
        "embedding_note": embedding_note,
        "init_failures": [
            {"language": key[0], "repo": key[2], "detail": init_detail.get(key, "")}
            for key, ok in init_ok.items()
            if not ok
        ],
        **(run_meta or {}),
    }
    report = report_module.build_report(scoring, raws, meta)
    json_path, md_path = report_module.write_reports(workdir_path, report)

    if progress:
        _stderr(
            f"[runner] 完成：{total} 题 / accuracy {report['aggregate']['all']['accuracy'] * 100:.1f}%"
            f"（executed={counter['executed']} skipped={counter['skipped']}）"
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
    )


def _now_iso() -> str:
    return datetime.now(tz=timezone.utc).isoformat(timespec="seconds")


def score_only(
    tasks: Sequence[Task],
    dataset: dict[str, list[dict[str, Any]]],
    workdir: Path | str,
    threshold: float = scorer.DEFAULT_THRESHOLD,
    extra_meta: dict[str, Any] | None = None,
) -> RunSummary:
    """只用已有 raw 结果重新判分（不重跑检索，``score`` 子命令用）。

    与 ``run`` 共用同一套聚合/出报告代码，保证两种入口产出一致。
    """
    workdir_path = Path(workdir)
    missing = [task.id for task in tasks if not _raw_path(workdir_path, task.id).is_file()]
    if missing:
        raise FileNotFoundError(
            f"{len(missing)} 道题缺少 raw 结果（例如 {missing[0]}）；先运行 `run` 或加 --limit-questions 缩小范围"
        )

    raws = {task.id: _read_raw(_raw_path(workdir_path, task.id)) or {} for task in tasks}
    outputs_by_task_id = {task.id: list(raws[task.id].get("outputs", [])) for task in tasks}
    scoring = scorer.score_tasks(tasks, outputs_by_task_id, dataset, threshold=threshold)

    mode_counts: dict[str, int] = {}
    for payload in raws.values():
        mode = payload.get("mode") or adapter.MODE_UNKNOWN
        mode_counts[mode] = mode_counts.get(mode, 0) + 1

    meta = {
        "generated_at": _now_iso(),
        "tasks": len(tasks),
        "repos": len({_repo_key(task) for task in tasks}),
        "threshold": threshold,
        "bleu_backend": scoring["bleu_backend"],
        "treesitter_available": scorer.treesitter_available(),
        "mode_counts": mode_counts,
        "rescore_only": True,
        **(extra_meta or {}),
    }
    report = report_module.build_report(scoring, raws, meta)
    json_path, md_path = report_module.write_reports(workdir_path, report)

    accuracy = report["aggregate"]["all"]["accuracy"]
    return RunSummary(
        total_tasks=len(tasks),
        executed=0,
        skipped=len(tasks),
        init_failures=0,
        mode_counts=mode_counts,
        report=report,
        report_json_path=str(json_path),
        report_md_path=str(md_path),
    )
