"""命令行入口：``prepare`` / ``run`` / ``score`` / ``selftest``。

用法（在 ``evals/mtebcode/`` 下执行）：

.. code-block:: bash

    python -m mtebcode_eval prepare --workdir /tmp/mtebcode-eval \\
        --tasks csn \\
        --corpus-size 300 --proxy http://127.0.0.1:7897
    python -m mtebcode_eval run   --workdir /tmp/mtebcode-eval --limit 20 --concurrency 4
    python -m mtebcode_eval score --workdir /tmp/mtebcode-eval --k 10
    python -m mtebcode_eval selftest      # 完全离线，不依赖网络 / codeseek 二进制

所有产物都落在 ``--workdir``（默认 ``/tmp/mtebcode-eval``），不污染仓库。

退出码：

* ``0`` 成功；
* ``2`` 前置条件缺失（``tasks.jsonl`` 不存在 / 找不到二进制 / ``--require-embedding`` 未满足 /
  任务名非法，或数据源不可达）。
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any, Sequence

from . import adapter, data, runner, scorer, selftest

__all__ = ["main"]

DEFAULT_WORKDIR = "/tmp/mtebcode-eval"

#: ``--tasks csn`` 别名：展开为 100 个 CodeSearchNetRetrieval 语言分片任务名。
TASKS_ALIAS_CSN = "csn"


# ---------------------------------------------------------------------------
# 公共小工具
# ---------------------------------------------------------------------------


def _parse_tasks(raw: str) -> list[str]:
    """解析 ``--tasks``：逗号分隔的任务名，``csn`` 展开为全部 100 个 CSN 分片。"""
    values: list[str] = []
    for item in raw.split(","):
        token = item.strip()
        if not token:
            continue
        if token.lower() == TASKS_ALIAS_CSN:
            values.extend(data.CSN_TASK_NAMES)
        else:
            values.append(token)
    ordered: list[str] = []
    for value in values:
        if value not in ordered:
            ordered.append(value)
    values = ordered
    unknown = [value for value in values if value not in data.TASKS]
    if unknown:
        raise argparse.ArgumentTypeError(
            f"未知任务 {unknown}；可选：{', '.join(data.TASKS)}"
        )
    unverified = [value for value in values if data.TASKS[value].unverified]
    if unverified:
        raise argparse.ArgumentTypeError(
            f"任务 {unverified} 在注册表里被标记 unverified（config/split 未实测确认），"
            "请先补齐 mtebcode_eval/data.py 的 TASKS"
        )
    excluded = [value for value in values if data.TASKS[value].excluded_reason]
    if excluded:
        detail = "; ".join(f"{value}: {data.TASKS[value].excluded_reason}" for value in excluded)
        raise argparse.ArgumentTypeError(
            f"任务 {excluded} 已被实测剔除（excluded_reason），不能选中：{detail}"
        )
    return values


def _limit_tasks_per_task(tasks: Sequence[data.Task], limit: int) -> list[data.Task]:
    """``--limit-queries``：**每个任务**最多保留前 N 条 query（0 = 全部）。"""
    if limit <= 0:
        return list(tasks)
    seen: dict[str, int] = {}
    kept: list[data.Task] = []
    for task in tasks:
        used = seen.get(task.task, 0)
        if used >= limit:
            continue
        seen[task.task] = used + 1
        kept.append(task)
    return kept


def _prepare_warnings(workdir_meta: dict[str, Any]) -> list[str]:
    """把 prepare 阶段各任务记录的告警汇总成一个列表（报告里会置顶）。"""
    collected: list[str] = []
    for entry in workdir_meta.get("tasks") or []:
        for warning in entry.get("warnings") or []:
            collected.append(f"{entry.get('task')}: {warning}")
    return collected


def _print_summary(summary: runner.RunSummary, k: int) -> None:
    aggregate = summary.report.get("aggregate") or {}
    mean = aggregate.get("mean_ndcg_at_10")
    shown = "n/a" if mean is None else f"{float(mean):.4f}"
    print("")
    print(f"总体 Mean nDCG@{k}：{shown}")
    for task, bucket in (aggregate.get("by_task") or {}).items():
        value = bucket.get("ndcg_at_10")
        text = "n/a" if value is None else f"{float(value):.4f}"
        print(
            f"  - {task:<28} nDCG@{k}={text}  "
            f"recall@10={_show(bucket.get('recall_at_10'))}  "
            f"({bucket.get('n_scored')}/{bucket.get('n_queries')} queries)"
        )
    print(f"检索模式：{summary.mode_counts}")
    if summary.warnings:
        print(f"警示 {len(summary.warnings)} 条（报告顶部有完整列表）：")
        for warning in summary.warnings[:3]:
            print(f"  * {warning[:160]}")
    print(f"report.json：{summary.report_json_path}")
    print(f"report.md  ：{summary.report_md_path}")


def _show(value: Any) -> str:
    return "n/a" if value is None else f"{float(value):.4f}"


# ---------------------------------------------------------------------------
# prepare
# ---------------------------------------------------------------------------


def cmd_prepare(args: argparse.Namespace) -> int:
    workdir = Path(args.workdir).expanduser()
    workdir.mkdir(parents=True, exist_ok=True)

    specs = [data.TASKS[name] for name in args.tasks]
    client = data.DatasetClient(
        workdir,
        proxy=args.proxy,
        retries=args.retries,
        rate_limit=args.rate_limit,
        cache_only=args.cache_only,
        quiet=args.quiet,
        parquet_cache=args.parquet_cache,
    )
    if args.parquet_cache:
        print(f"[prepare] 本地 parquet 数据源：{args.parquet_cache}（命中即不发网络请求）")
    if client.proxy:
        print(f"[prepare] 代理：{client.proxy}")
    elif not args.quiet:
        print("[prepare] 未指定代理，将尝试直连（国内网络通常需要 --proxy）", file=sys.stderr)

    options = data.PrepareOptions(
        workdir=workdir,
        corpus_size=max(1, args.corpus_size),
        limit_queries=max(0, args.limit_queries),
        seed=args.seed,
        force=args.force,
        quiet=args.quiet,
    )

    tasks: list[data.Task] = []
    task_meta: list[dict[str, Any]] = []
    failures: list[str] = []

    for spec in specs:
        print(f"\n[prepare] === {spec.name} ({spec.hf_dataset}) ===")
        try:
            prepared = data.prepare_task(spec, client, options)
            dataset_size = _dataset_size(client, spec)
            if dataset_size:
                prepared.meta["dataset_size"] = dataset_size
            tasks.extend(prepared.tasks)
            task_meta.append(prepared.meta)
            for warning in prepared.meta.get("warnings") or []:
                print(f"[prepare] WARN {spec.name}: {warning}", file=sys.stderr)
        except data.DatasetError as exc:
            failures.append(f"{spec.name}: {exc}")
            print(f"[prepare] FAIL {spec.name}: {exc}", file=sys.stderr)
        except (OSError, ValueError) as exc:
            failures.append(f"{spec.name}: {type(exc).__name__}: {exc}")
            print(f"[prepare] FAIL {spec.name}: {type(exc).__name__}: {exc}", file=sys.stderr)

    if not tasks:
        print(
            "[prepare] 没有任何任务准备成功，无法继续。请检查网络/代理与任务名。",
            file=sys.stderr,
        )
        for failure in failures:
            print(f"  - {failure}", file=sys.stderr)
        return 2

    tasks_path = data.write_tasks(tasks, workdir / data.TASKS_FILE)
    # 共享语料的任务共用同一个 corpora/<corpus_key>，因此这里按 corpus_dir 去重后再求和，
    # 否则 100 个 CSN 分片会把 3 份 1,000 文档的语料重复计成 100,000。
    pool_by_corpus: dict[str, int] = {}
    for entry in task_meta:
        key = str(entry.get("corpus_dir") or entry.get("task"))
        pool_by_corpus[key] = max(pool_by_corpus.get(key, 0), int(entry.get("num_pool_docs") or 0))
    meta = {
        **data.benchmark_meta_header("huggingface-datasets-server", "prepare"),
        "workdir": str(workdir),
        "seed": options.seed,
        "corpus_size": options.corpus_size,
        "limit_queries": options.limit_queries,
        "task_order": [spec.name for spec in specs if spec.name in {t.task for t in tasks}],
        "tasks": task_meta,
        "n_tasks": len({task.task for task in tasks}),
        "n_queries": len(tasks),
        "n_docs_total": sum(pool_by_corpus.values()),
        "sampling_fingerprint": data.sampling_fingerprint(
            tasks, options.seed, options.corpus_size, options.limit_queries
        ),
        "scoring_protocol": {
            "primary_metric": "ndcg_at_10",
            "aggregation": "mean_of_task_means",
            "deduplicate_doc_ids": True,
            "skip_queries_without_positive": True,
        },
        "client_stats": client.stats.to_dict(),
        "data_sources": {
            "parquet_cache": args.parquet_cache,
            "parquet_pages": client.stats.parquet_pages,
            "rest_requests": client.stats.requests,
        },
        "failures": failures,
    }
    meta_path = data.write_workdir_meta(workdir, meta)

    print("\n[prepare] 完成")
    for entry in task_meta:
        print(
            f"  - {entry['task']:<28} queries={entry['num_queries_sampled']:<3} "
            f"pool={entry['num_pool_docs']:<4} gold={entry['num_gold_docs']:<4} "
            f"qrels={entry['qrels_config']}/{entry['qrels_split']} "
            f"corpus={entry['num_corpus_rows']}"
        )
    print(f"题目：{tasks_path}")
    print(f"元信息：{meta_path}")
    print(f"语料目录：{workdir / data.CORPORA_DIR}")
    if failures:
        print(f"[prepare] 有 {len(failures)} 个任务失败（其余任务照常写出）：", file=sys.stderr)
        for failure in failures:
            print(f"  - {failure}", file=sys.stderr)
    return 0


def _dataset_size(client: data.DatasetClient, spec: data.TaskSpec) -> dict[str, Any]:
    """``/size`` 规模统计（失败不算错误，只是不写进 meta）。"""
    try:
        payload = client.size(spec.hf_dataset)
    except data.DatasetError:
        return {}
    size = payload.get("size") or {}
    dataset = size.get("dataset") or {}
    return {
        "num_rows_total": dataset.get("num_rows"),
        "num_bytes_original_files": dataset.get("num_bytes_original_files"),
        "configs": [
            {"config": entry.get("config"), "num_rows": entry.get("num_rows")}
            for entry in (size.get("configs") or [])
        ][:12],
    }


def _load_partial(workdir: Path, spec: data.TaskSpec) -> list[data.Task]:
    """（保留给调试用）从一个已有的 ``tasks.jsonl`` 里按任务名过滤题目。"""
    final = workdir / data.TASKS_FILE
    if not final.is_file():
        return []
    return [task for task in data.load_tasks(final) if task.task == spec.name]


# ---------------------------------------------------------------------------
# run
# ---------------------------------------------------------------------------


def cmd_run(args: argparse.Namespace) -> int:
    workdir = Path(args.workdir).expanduser()
    try:
        tasks_all = data.load_tasks(workdir / data.TASKS_FILE)
    except (FileNotFoundError, ValueError, json.JSONDecodeError) as exc:
        print(f"[run] {exc}", file=sys.stderr)
        return 2

    tasks = _limit_tasks_per_task(tasks_all, args.limit_queries)
    if not tasks:
        print("[run] 没有题目可跑（先运行 `prepare`）", file=sys.stderr)
        return 2
    if args.limit_queries:
        print(f"[run] 冒烟模式：每任务最多 {args.limit_queries} 条 query，共 {len(tasks)} 条")

    embedding_ok, embedding_note = adapter.check_embedding_configured()
    print(f"[run] embedding 检测：{embedding_note}")
    if args.require_embedding and not embedding_ok:
        print(
            "[run] --require-embedding 已开启，但语义检索未配置，评测会退化为函数名子串匹配，"
            "因此直接退出。\n"
            "      修复：在 ~/.codeseek/config.json 的 embedding.api_token 填入可用 token 后重试。",
            file=sys.stderr,
        )
        return 2
    if not embedding_ok:
        print(
            "[run] ⚠️ 未启用语义检索：本次分数衡量的是函数名子串匹配能力，报告顶部会给出醒目警示。",
            file=sys.stderr,
        )

    try:
        binary = adapter.ensure_binary(args.codeseek_bin, repo_root=args.repo_root, auto_build=args.auto_build)
    except (FileNotFoundError, RuntimeError) as exc:
        print(f"[run] {exc}", file=sys.stderr)
        return 2
    print(f"[run] codeseek 二进制：{binary}")

    workdir_meta = data.read_workdir_meta(workdir)
    summary = runner.run(
        tasks=tasks,
        workdir=workdir,
        binary=binary,
        limit=args.limit,
        k=args.k,
        concurrency=args.concurrency,
        timeout=args.timeout,
        init_timeout=args.init_timeout,
        force=args.force,
        embedding_configured=embedding_ok,
        embedding_note=embedding_note,
        run_meta={
            "codeseek_binary": adapter.describe_binary(binary),
            "repo_root": str(Path(args.repo_root).expanduser()) if args.repo_root else str(adapter.default_repo_root()),
            "seed": workdir_meta.get("seed"),
            "corpus_size": workdir_meta.get("corpus_size"),
            "limit_queries": args.limit_queries or workdir_meta.get("limit_queries"),
            "sampling_fingerprint": workdir_meta.get("sampling_fingerprint"),
            "prepare_warnings": _prepare_warnings(workdir_meta),
        },
        progress=not args.quiet,
    )
    _print_summary(summary, args.k)
    return 0


# ---------------------------------------------------------------------------
# score
# ---------------------------------------------------------------------------


def cmd_score(args: argparse.Namespace) -> int:
    workdir = Path(args.workdir).expanduser()
    try:
        tasks_all = data.load_tasks(workdir / data.TASKS_FILE)
    except (FileNotFoundError, ValueError, json.JSONDecodeError) as exc:
        print(f"[score] {exc}", file=sys.stderr)
        return 2

    tasks = _limit_tasks_per_task(tasks_all, args.limit_queries)

    binary: Path | None = None
    try:
        binary = adapter.find_binary(args.codeseek_bin, repo_root=args.repo_root)
    except FileNotFoundError:
        binary = None

    embedding_ok, embedding_note = adapter.check_embedding_configured()
    workdir_meta = data.read_workdir_meta(workdir)
    try:
        summary = runner.score_only(
            tasks=tasks,
            workdir=workdir,
            k=args.k,
            search_limit=args.limit,
            extra_meta={
                "codeseek_binary": adapter.describe_binary(binary) if binary is not None else None,
                "seed": workdir_meta.get("seed"),
                "corpus_size": workdir_meta.get("corpus_size"),
                "limit_queries": args.limit_queries or workdir_meta.get("limit_queries"),
                "sampling_fingerprint": workdir_meta.get("sampling_fingerprint"),
                "embedding_configured": embedding_ok,
                "embedding_note": embedding_note,
                "prepare_warnings": _prepare_warnings(workdir_meta),
            },
        )
    except FileNotFoundError as exc:
        print(f"[score] {exc}", file=sys.stderr)
        return 2

    _print_summary(summary, args.k)
    return 0


# ---------------------------------------------------------------------------
# selftest
# ---------------------------------------------------------------------------


def _cmd_selftest(_args: argparse.Namespace) -> int:
    return selftest.run_selftest()


# ---------------------------------------------------------------------------
# argparse
# ---------------------------------------------------------------------------


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="python -m mtebcode_eval",
        description=(
            "用 MTEB(Code, v1) 风格检索任务评测 codeseek（主指标 nDCG@10，"
            "总体分 = 任务级均值的算术平均）"
        ),
    )
    sub = parser.add_subparsers(dest="command", required=True)

    prepare = sub.add_parser("prepare", help="抓数据 + 抽样 query + 池化 corpus + 物化 + 写 tasks.jsonl")
    prepare.add_argument("--workdir", default=DEFAULT_WORKDIR, help=f"工作目录（默认 {DEFAULT_WORKDIR}）")
    prepare.add_argument(
        "--tasks",
        type=_parse_tasks,
        default=list(data.DEFAULT_TASKS),
        help=(
            f"逗号分隔的任务名，默认 {','.join(data.DEFAULT_TASKS)}；"
            f"别名 `{TASKS_ALIAS_CSN}` = 全部 {len(data.CSN_TASK_NAMES)} 个 "
            f"CodeSearchNetRetrieval 语言分片（go 34 + java 33 + python 33）；"
            f"其他可选：{','.join(name for name in data.TASKS if name not in data.CSN_TASK_NAMES)}"
        ),
    )
    prepare.add_argument(
        "--corpus-size",
        type=int,
        default=300,
        help="每个任务的语料池大小（文档数，默认 300）；full_corpus 任务（CSN 分片）忽略此项，物化整份语料",
    )
    prepare.add_argument("--limit-queries", type=int, default=30, help="每个任务抽多少条 query（默认 30，0=全部）")
    prepare.add_argument("--seed", type=int, default=42, help="抽样随机种子（默认 42，保证可复现）")
    prepare.add_argument("--proxy", default=None, help="下载代理，如 http://127.0.0.1:7897（默认读 https_proxy）")
    prepare.add_argument(
        "--parquet-cache",
        default=None,
        help=(
            "本地 HF parquet 缓存根目录（如 ~/.cache/huggingface/hub）：某 (config, split) 命中本地 "
            "parquet 时直接读本地、不发网络请求。用于绕开 datasets-server /rows 的 429。"
        ),
    )
    prepare.add_argument("--cache-only", action="store_true", help="只用已缓存的页，缓存缺失即报错（离线复现）")
    prepare.add_argument("--rate-limit", type=float, default=0.15, help="每页请求之间的间隔秒数（默认 0.15）")
    prepare.add_argument("--retries", type=int, default=4, help="单页请求失败重试次数（默认 4）")
    prepare.add_argument("--force", action="store_true", help="强制重写语料文件与 docmap")
    prepare.add_argument("--quiet", action="store_true", help="不打印每页缓存/进度信息")
    prepare.set_defaults(func=cmd_prepare)

    run_parser = sub.add_parser("run", help="执行检索评测并产出 report.json / report.md")
    run_parser.add_argument("--workdir", default=DEFAULT_WORKDIR)
    run_parser.add_argument(
        "--codeseek-bin",
        default=None,
        help="codeseek 二进制路径（默认查 CODESEEK_BIN / rust-core/target/release）",
    )
    run_parser.add_argument("--repo-root", default=None, help="仓库根（用于定位二进制，默认自动推断）")
    run_parser.add_argument("--limit", type=int, default=20, help="codeseek search --limit（默认 20）")
    run_parser.add_argument("--k", type=int, default=scorer.DEFAULT_K, help="主指标 nDCG@k 的 k（默认 10）")
    run_parser.add_argument(
        "--limit-queries", type=int, default=0, help="每个任务最多跑 N 条 query（0=全部，冒烟用）"
    )
    run_parser.add_argument("--concurrency", type=int, default=4, help="search 并发线程数（默认 4）")
    run_parser.add_argument("--timeout", type=float, default=180.0, help="单题 search 超时秒数（默认 180）")
    run_parser.add_argument("--init-timeout", type=float, default=1800.0, help="单语料 init 超时秒数（默认 1800）")
    run_parser.add_argument("--force", action="store_true", help="忽略已有 raw 结果，强制重跑检索")
    run_parser.add_argument("--auto-build", action="store_true", help="找不到二进制时自动 cargo build --release")
    run_parser.add_argument(
        "--require-embedding", action="store_true", help="未配置 embedding.api_token 时直接退出（退出码 2）"
    )
    run_parser.add_argument("--quiet", action="store_true", help="不打印每题进度")
    run_parser.set_defaults(func=cmd_run)

    score = sub.add_parser("score", help="只用已有 raw 结果重新评分（不重跑检索）")
    score.add_argument("--workdir", default=DEFAULT_WORKDIR)
    score.add_argument("--limit", type=int, default=20, help="仅用于 recall@k 的口径（与 run --limit 一致）")
    score.add_argument("--k", type=int, default=scorer.DEFAULT_K, help="主指标 nDCG@k 的 k（默认 10）")
    score.add_argument("--limit-queries", type=int, default=0, help="每个任务最多评 N 条 query（0=全部）")
    score.add_argument("--codeseek-bin", default=None, help="可选：仅用于在报告里记录二进制指纹")
    score.add_argument("--repo-root", default=None)
    score.set_defaults(func=cmd_score)

    selftest_parser = sub.add_parser(
        "selftest", help="离线自检：nDCG 数值 + 容错解析 + doc_id 反查 + 端到端报告（不需要网络/二进制）"
    )
    selftest_parser.set_defaults(func=_cmd_selftest)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    try:
        return int(args.func(args))
    except KeyboardInterrupt:
        print("\n[main] 中断", file=sys.stderr)
        return 130
