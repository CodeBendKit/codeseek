"""报告输出：``report.json``（机器可读）+ ``report.md``（英文可读）。

``report.md`` 必须包含（与 README 的"口径说明"对应）：

1. Summary（总体 accuracy + 按语言表）；
2. **Retrieval mode warning**：``name_fallback`` 占比 > 0 时置顶 blockquote，明确说明
   "本次评测未能启用语义检索，codeseek 退化为函数名子串匹配，分数不代表其语义检索能力"；
3. Breakdown by Language × Repo 细分表；
4. Top 10 Failures（按 ``best_similarity`` 从高到低，帮你看"差一点"的题）；
5. Reproduction Commands。

分数统一保留 **1 位小数**（与官方榜一致），并在表头注明 threshold。
"""

from __future__ import annotations

import json
from collections import OrderedDict
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Mapping

from .data import REPORT_JSON, REPORT_MD

__all__ = ["build_report", "render_markdown", "write_reports"]


def _percent(value: float) -> str:
    """accuracy 展示为 1 位小数的百分数。"""
    return f"{value * 100:.1f}%"


def build_report(
    scoring: dict[str, Any],
    raws: Mapping[str, dict[str, Any]],
    meta: dict[str, Any],
) -> dict[str, Any]:
    """把判分结果 + raw 元信息合成最终报告 dict。

    :param scoring: ``scorer.score_tasks`` 的返回值。
    :param raws: ``task_id -> raw 结果``（提供 mode / elapsed / error / results_count）。
    :param meta: run 阶段的元信息（二进制、数据集指纹、并发度等）。
    """
    per_task: list[dict[str, Any]] = []
    for row in scoring["per_task"]:
        raw = raws.get(row["id"], {})
        per_task.append(
            {
                **row,
                "mode": raw.get("mode"),
                "results_count": raw.get("results_count", 0),
                "search_elapsed_s": raw.get("elapsed_s", 0.0),
                "search_error": (raw.get("error") or "")[:300],
                "retrieved_at": raw.get("retrieved_at", ""),
                "top_symbols": [item.get("symbol") for item in (raw.get("results_meta") or [])[:3]],
            }
        )

    aggregate = {
        "all": _with_percent(scoring["aggregate"]["all"]),
        "by_language": OrderedDict(
            (language, _with_percent(value))
            for language, value in scoring["aggregate"]["by_language"].items()
        ),
    }

    by_repo: "OrderedDict[str, dict[str, Any]]" = OrderedDict()
    for row in per_task:
        key = f"{row['language']}/{row['repo']}"
        bucket = by_repo.setdefault(
            key,
            {
                "language": row["language"],
                "repo": row["repo"],
                "total": 0,
                "passed": 0,
                "modes": {},
            },
        )
        bucket["total"] += 1
        bucket["passed"] += 1 if row.get("passed") else 0
        mode = row.get("mode") or "unknown_empty"
        bucket["modes"][mode] = bucket["modes"].get(mode, 0) + 1
    for bucket in by_repo.values():
        bucket["accuracy"] = bucket["passed"] / bucket["total"] if bucket["total"] else 0.0
        bucket["accuracy_percent"] = _percent(bucket["accuracy"])

    failures = sorted(
        (row for row in per_task if not row.get("passed")),
        key=lambda row: row.get("best_similarity", 0.0),
        reverse=True,
    )

    mode_counts = dict(meta.get("mode_counts") or {})
    total = len(per_task)
    fallback = int(mode_counts.get("name_fallback", 0))
    unknown = int(mode_counts.get("unknown_empty", 0))
    hybrid = int(mode_counts.get("hybrid", 0))
    fallback_ratio = (fallback / total) if total else 0.0

    return {
        "meta": {
            **meta,
            "generated_at": meta.get("generated_at")
            or datetime.now(tz=timezone.utc).isoformat(timespec="seconds"),
            "mode_counts": mode_counts,
            "mode_ratio": {
                "hybrid": (hybrid / total) if total else 0.0,
                "name_fallback": fallback_ratio,
                "unknown_empty": (unknown / total) if total else 0.0,
            },
        },
        "aggregate": aggregate,
        "by_repo": list(by_repo.values()),
        "per_task": per_task,
        "failure_top": failures[:10],
        "warnings": _warnings(meta, mode_counts, fallback_ratio),
    }


def _with_percent(aggregate: dict[str, Any]) -> dict[str, Any]:
    """给聚合结果补一个 ``accuracy_percent``（1 位小数）。"""
    return {
        **aggregate,
        "accuracy_percent": _percent(float(aggregate.get("accuracy", 0.0))),
    }


def _warnings(
    meta: Mapping[str, Any], mode_counts: Mapping[str, int], fallback_ratio: float
) -> list[str]:
    """生成需要在报告顶部醒目标注的警示。"""
    warnings: list[str] = []
    if mode_counts.get("name_fallback", 0) > 0:
        warnings.append(
            f"{mode_counts['name_fallback']} tasks ({fallback_ratio * 100:.1f}%) went through "
            "`name_fallback`: semantic retrieval was not enabled, so codeseek degraded to "
            "**function-name substring matching**. Scores on these tasks do **not** reflect its "
            "semantic retrieval ability and are not directly comparable to the official leaderboard."
        )
    if mode_counts.get("unknown_empty", 0) > 0:
        warnings.append(
            f"{mode_counts['unknown_empty']} tasks returned empty search results "
            "(`unknown_empty`), so the retrieval path cannot be determined from the JSON structure; "
            "check these tasks' error messages and whether the index was built."
        )
    if meta.get("embedding_configured") is False:
        warnings.append(
            "Config check: `"
            + str(meta.get("embedding_note") or "embedding not configured")
            + "`. Fix: put a usable token in `embedding.api_token` of `~/.codeseek/config.json`, "
            "then re-run `codeseek init` (or re-run this evaluation with `--force`)."
        )
    init_failures = meta.get("init_failures") or []
    if init_failures:
        names = ", ".join(f"{item['language']}/{item['repo']}" for item in init_failures[:5])
        warnings.append(
            f"{len(init_failures)} repositories failed to build an index ({names}); "
            "all of their tasks are recorded as failures. See `meta.init_failures` for details."
        )
    if meta.get("bleu_backend") == "builtin":
        warnings.append(
            "BLEU used the **built-in reimplementation** (nltk not installed). Differences from the "
            "official `repoqa.compute_score` are limited to nltk version-level details; run "
            "`pip install nltk` and then `score` the same raw results to double-check."
        )
    return warnings


def render_markdown(report: dict[str, Any]) -> str:
    """把报告 dict 渲染成英文 markdown。"""
    meta = report["meta"]
    all_agg = report["aggregate"]["all"]
    threshold = meta.get("threshold")
    lines: list[str] = []

    lines.append("# RepoQA × codeseek Evaluation Report")
    lines.append("")
    lines.append(
        f"- Generated at: `{meta.get('generated_at')}`  Tasks: **{meta.get('tasks')}**  "
        f"Repositories: **{meta.get('repos')}**"
    )
    lines.append(
        f"- Scoring: BLEU-4 + Chen&Cherry method4 (backend=`{meta.get('bleu_backend')}`), "
        "candidate set = the 10 needles of that repo, threshold "
        f"**{threshold}** (a task passes only if the BLEU-best candidate is the ground truth)"
    )
    if meta.get("dataset"):
        dataset = meta["dataset"]
        lines.append(
            f"- Dataset: `{dataset.get('release')}` (`sha256:{dataset.get('sha256_prefix')}...`, "
            f"{dataset.get('size_bytes', 0)} bytes)"
        )
    if meta.get("codeseek_binary"):
        binary = meta["codeseek_binary"]
        lines.append(
            f"- Binary under test: `{binary.get('path')}` "
            f"(mtime={binary.get('mtime_iso')}, sha256:{binary.get('sha256_prefix')}...)"
        )
    lines.append("")

    warnings = report.get("warnings") or []
    if warnings:
        for warning in warnings:
            lines.append(f"> ⚠️ {warning}")
            lines.append(">")
        lines.append("")

    # ---------- 1. Summary ----------
    lines.append("## 1. Summary")
    lines.append("")
    lines.append(
        f"**Overall accuracy: {all_agg['accuracy_percent']}** "
        f"({all_agg['passed']}/{all_agg['total']} tasks passed, threshold={threshold})"
    )
    lines.append("")
    lines.append("| Language | Passed/Total | accuracy |")
    lines.append("| --- | --- | --- |")
    for language, agg in report["aggregate"]["by_language"].items():
        lines.append(f"| {language} | {agg['passed']}/{agg['total']} | {agg['accuracy_percent']} |")
    lines.append(f"| **all** | {all_agg['passed']}/{all_agg['total']} | {all_agg['accuracy_percent']} |")
    lines.append("")

    # ---------- 2. Retrieval mode statistics ----------
    mode_counts = meta.get("mode_counts") or {}
    ratio = meta.get("mode_ratio") or {}
    lines.append("## 2. Retrieval Mode Statistics")
    lines.append("")
    lines.append("| Mode | Tasks | Share | Meaning |")
    lines.append("| --- | --- | --- | --- |")
    lines.append(
        f"| hybrid | {mode_counts.get('hybrid', 0)} | {ratio.get('hybrid', 0.0) * 100:.1f}% | "
        "true semantic retrieval (LanceDB + BM25, results include `code_block`) |"
    )
    lines.append(
        f"| name_fallback | {mode_counts.get('name_fallback', 0)} | "
        f"{ratio.get('name_fallback', 0.0) * 100:.1f}% | "
        "degrades to function-name substring matching when `embedding.api_token` is absent "
        "(no `code_block`) |"
    )
    lines.append(
        f"| unknown_empty | {mode_counts.get('unknown_empty', 0)} | "
        f"{ratio.get('unknown_empty', 0.0) * 100:.1f}% | "
        "empty or unparsable search results, cannot be judged |"
    )
    lines.append("")
    lines.append(f"- Embedding config check: `{meta.get('embedding_note') or 'not checked'}`")
    lines.append(
        "- tree-sitter (optional, used to extract the first function definition): "
        f"`{meta.get('treesitter_available')}`"
    )
    lines.append("")

    # ---------- 3. Breakdown by language × repo ----------
    lines.append("## 3. Breakdown by Language × Repo")
    lines.append("")
    lines.append("| Language | repo | Passed/Total | accuracy | Retrieval mode |")
    lines.append("| --- | --- | --- | --- | --- |")
    for bucket in report.get("by_repo", []):
        modes = ", ".join(f"{k}×{v}" for k, v in sorted(bucket["modes"].items()))
        lines.append(
            f"| {bucket['language']} | {bucket['repo']} | "
            f"{bucket['passed']}/{bucket['total']} | {bucket['accuracy_percent']} | {modes} |"
        )
    lines.append("")

    # ---------- 4. Top 10 failures ----------
    lines.append("## 4. Top 10 Failures (sorted by best_similarity, descending)")
    lines.append("")
    failures = report.get("failure_top") or []
    if not failures:
        lines.append("No failing tasks.")
    else:
        lines.append(
            "| # | Language | repo | needle (ground truth) | best_target | sim | mode | reason |"
        )
        lines.append("| --- | --- | --- | --- | --- | --- | --- | --- |")
        for index, row in enumerate(failures, start=1):
            lines.append(
                f"| {index} | {row['language']} | {row['repo']} | `{row['needle_name']}` | "
                f"`{row.get('best_target')}` | {row.get('best_similarity', 0.0):.3f} | "
                f"{row.get('mode')} | {row.get('reason')} |"
            )
        lines.append("")
        lines.append(
            "> The `sim` column keeps 3 decimals (to show near-misses); "
            "accuracy in the aggregate keeps 1 decimal."
        )
    lines.append("")

    # ---------- 5. Reproduction ----------
    lines.append("## 5. Reproduction Commands")
    lines.append("")
    lines.append("```bash")
    lines.append("cd evals/repoqa")
    lines.append("cargo build --release --manifest-path ../../rust-core/Cargo.toml")
    lines.append(
        "pip install -r requirements.txt   "
        "# optional: install nltk for the official BLEU implementation"
    )
    lines.append("python -m repoqa_eval prepare --workdir /tmp/repoqa-eval")
    lines.append(
        "python -m repoqa_eval run --workdir /tmp/repoqa-eval "
        f"--limit {meta.get('search_limit', 5)} --threshold {threshold} "
        f"--concurrency {meta.get('concurrency', 4)}"
    )
    lines.append(
        "python -m repoqa_eval score --workdir /tmp/repoqa-eval   "
        "# re-score with a different threshold, no re-run of retrieval"
    )
    lines.append("```")
    lines.append("")
    lines.append("---")
    lines.append("")
    lines.append(
        "See `evals/repoqa/README.md` for the scoring protocol and known limitations; "
        "scoring is implemented in `repoqa_eval/scorer.py` + `repoqa_eval/bleu.py` "
        "(a verbatim reimplementation of the official `repoqa/metric.py` and `compute_score.py`)."
    )
    lines.append("")
    return "\n".join(lines)


def write_reports(workdir: Path | str, report: dict[str, Any]) -> tuple[Path, Path]:
    """写出 ``report.json`` 与 ``report.md``，返回两个路径。"""
    root = Path(workdir)
    root.mkdir(parents=True, exist_ok=True)
    json_path = root / REPORT_JSON
    md_path = root / REPORT_MD

    json_path.write_text(
        json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8"
    )
    md_path.write_text(render_markdown(report), encoding="utf-8")
    return json_path, md_path
