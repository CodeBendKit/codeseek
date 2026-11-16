"""命令行入口：``prepare`` / ``run`` / ``score`` / ``selftest``。

用法（在 ``evals/repoqa/`` 下执行）：

.. code-block:: bash

    python -m repoqa_eval prepare --workdir /tmp/repoqa-eval
    python -m repoqa_eval run     --workdir /tmp/repoqa-eval --limit 5
    python -m repoqa_eval score   --workdir /tmp/repoqa-eval --threshold 0.7
    python -m repoqa_eval selftest      # 完全离线，不依赖网络 / codeseek / nltk

所有产物都落在 ``--workdir``（默认 ``/tmp/repoqa-eval``），不污染仓库。
"""

from __future__ import annotations

import argparse
import json
import sys
import tempfile
from pathlib import Path
from typing import Any, Sequence

from . import adapter, bleu, data, runner, scorer

__all__ = ["main"]

DEFAULT_WORKDIR = "/tmp/repoqa-eval"


# ---------------------------------------------------------------------------
# 公共小工具
# ---------------------------------------------------------------------------


def _parse_languages(raw: str) -> list[str]:
    values = [item.strip() for item in raw.split(",") if item.strip()]
    unknown = [value for value in values if value not in data.ALL_LANGUAGES]
    if unknown:
        raise argparse.ArgumentTypeError(
            f"未知语言 {unknown}；可选：{', '.join(data.ALL_LANGUAGES)}"
        )
    return values


def _load_dataset_for_workdir(workdir: Path) -> tuple[dict[str, Any], Path]:
    """根据 workdir/meta.json 里记录的数据集路径加载数据集。"""
    meta = data.read_workdir_meta(workdir)
    dataset_path = meta.get("dataset_path")
    if not dataset_path:
        raise FileNotFoundError(
            f"{workdir / data.WORKDIR_META} 中没有 dataset_path；请先运行 `prepare`"
        )
    path = Path(dataset_path)
    if not path.is_file():
        raise FileNotFoundError(f"数据集文件不存在：{path}（重新运行 `prepare`）")
    return data.load_dataset(path), path


def _meta_for_report(dataset_path: Path, binary: Path | None) -> dict[str, Any]:
    """报告 meta 里需要的数据集 / 二进制指纹。"""
    meta: dict[str, Any] = {"dataset": data.dataset_fingerprint(dataset_path)}
    if binary is not None:
        meta["codeseek_binary"] = adapter.describe_binary(binary)
    return meta


def _subset_meta(workdir: Path) -> dict[str, Any]:
    """子集口径（写进 report.meta，便于说明"这个分数是在多少题上算的"）。"""
    workdir_meta = data.read_workdir_meta(workdir)
    return {
        "languages": workdir_meta.get("languages"),
        "repos_per_language": workdir_meta.get("repos_per_language"),
        "needles_per_repo": data.NEEDLES_PER_REPO,
        "position_ratio_formula": "(i + 0.5) / 10",
    }


def _print_summary(summary: runner.RunSummary) -> None:
    all_agg = summary.report["aggregate"]["all"]
    print("")
    print(f"总体 accuracy：{all_agg['accuracy_percent']}（{all_agg['passed']}/{all_agg['total']}）")
    for language, agg in summary.report["aggregate"]["by_language"].items():
        print(f"  - {language:<12} {agg['accuracy_percent']:>6}（{agg['passed']}/{agg['total']}）")
    print(f"检索模式：{summary.mode_counts}")
    print(f"report.json：{summary.report_json_path}")
    print(f"report.md  ：{summary.report_md_path}")


# ---------------------------------------------------------------------------
# prepare
# ---------------------------------------------------------------------------


def cmd_prepare(args: argparse.Namespace) -> int:
    workdir = Path(args.workdir).expanduser()
    workdir.mkdir(parents=True, exist_ok=True)
    languages = args.languages or list(data.DEFAULT_LANGUAGES)

    dataset_path = data.download_dataset(workdir, proxy=args.proxy)
    dataset = data.load_dataset(dataset_path)

    tasks = data.select_subset(dataset, languages, repos_per_language=args.repos_per_language)
    print(
        f"[prepare] 子集：{len(languages)} 语言 × {args.repos_per_language} repo × "
        f"{data.NEEDLES_PER_REPO} needle = {len(tasks)} 题"
    )

    # 落盘仓库快照（同一 repo 只落一次）。
    seen: set[tuple[str, int]] = set()
    git_failures: list[str] = []
    for task in tasks:
        key = (task.language, task.repo_index)
        if key in seen:
            continue
        seen.add(key)
        repo_entry = dataset[task.language][task.repo_index]
        destination = data.repo_dir(workdir, task.language, task.repo)
        ok, detail = data.materialize_repo(repo_entry, destination, force=args.force)
        status = "OK" if ok else "WARN"
        print(f"[prepare] {status} {task.language}/{task.repo} → {destination}（{detail}）")
        if not ok:
            git_failures.append(f"{task.language}/{task.repo}: {detail}")

    tasks_path = data.write_tasks(tasks, workdir / data.TASKS_FILE)
    data.write_workdir_meta(
        workdir,
        {
            "dataset_path": str(dataset_path),
            "dataset": data.dataset_fingerprint(dataset_path),
            "languages": languages,
            "repos_per_language": args.repos_per_language,
            "tasks": len(tasks),
            "tasks_file": str(tasks_path),
            "all_languages_in_release": list(data.ALL_LANGUAGES),
        },
    )
    print(f"[prepare] 题目已写出：{tasks_path}")
    print(f"[prepare] workdir：{workdir}")
    if git_failures:
        print(
            f"[prepare] 注意：{len(git_failures)} 个仓库 git init 失败，"
            "codeseek init 会因此报 `No project found`：",
            file=sys.stderr,
        )
        for item in git_failures:
            print(f"  - {item}", file=sys.stderr)
    return 0


# ---------------------------------------------------------------------------
# run
# ---------------------------------------------------------------------------


def cmd_run(args: argparse.Namespace) -> int:
    workdir = Path(args.workdir).expanduser()
    try:
        tasks_all = data.load_tasks(workdir / data.TASKS_FILE)
        dataset, dataset_path = _load_dataset_for_workdir(workdir)
    except (FileNotFoundError, ValueError) as exc:
        print(f"[run] {exc}", file=sys.stderr)
        return 2

    tasks = tasks_all[: args.limit_questions] if args.limit_questions else tasks_all
    if not tasks:
        print("[run] 没有题目可跑", file=sys.stderr)
        return 2
    if args.limit_questions:
        print(f"[run] 冒烟模式：只跑前 {len(tasks)} 题（共 {len(tasks_all)} 题）")

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
            "[run] ⚠️ 未启用语义检索：本次分数衡量的是函数名子串匹配能力，"
            "报告顶部会给出醒目警示。",
            file=sys.stderr,
        )

    try:
        binary = adapter.ensure_binary(
            args.codeseek_bin, repo_root=args.repo_root, auto_build=args.auto_build
        )
    except (FileNotFoundError, RuntimeError) as exc:
        print(f"[run] {exc}", file=sys.stderr)
        return 2
    print(f"[run] codeseek 二进制：{binary}")

    summary = runner.run(
        tasks=tasks,
        dataset=dataset,
        workdir=workdir,
        binary=binary,
        limit=args.limit,
        threshold=args.threshold,
        concurrency=args.concurrency,
        timeout=args.timeout,
        init_timeout=args.init_timeout,
        force=args.force,
        embedding_configured=embedding_ok,
        embedding_note=embedding_note,
        run_meta={
            **_meta_for_report(dataset_path, binary),
            "command": "run",
            "limit_questions": args.limit_questions,
            "subset": _subset_meta(workdir),
        },
        progress=not args.quiet,
    )
    _print_summary(summary)
    return 0


# ---------------------------------------------------------------------------
# score
# ---------------------------------------------------------------------------


def cmd_score(args: argparse.Namespace) -> int:
    workdir = Path(args.workdir).expanduser()
    try:
        tasks_all = data.load_tasks(workdir / data.TASKS_FILE)
        dataset, dataset_path = _load_dataset_for_workdir(workdir)
    except (FileNotFoundError, ValueError) as exc:
        print(f"[score] {exc}", file=sys.stderr)
        return 2
    tasks = tasks_all[: args.limit_questions] if args.limit_questions else tasks_all

    binary: Path | None = None
    try:
        binary = adapter.find_binary(args.codeseek_bin, repo_root=args.repo_root)
    except FileNotFoundError:
        binary = None

    try:
        summary = runner.score_only(
            tasks=tasks,
            dataset=dataset,
            workdir=workdir,
            threshold=args.threshold,
            extra_meta={
                **_meta_for_report(dataset_path, binary),
                "command": "score",
                "search_limit": args.limit,
                "subset": _subset_meta(workdir),
            },
        )
    except FileNotFoundError as exc:
        print(f"[score] {exc}", file=sys.stderr)
        return 2

    _print_summary(summary)
    return 0


# ---------------------------------------------------------------------------
# selftest
# ---------------------------------------------------------------------------


class _Checker:
    """收集自检结果（不依赖 pytest）。"""

    def __init__(self) -> None:
        self.failures: list[str] = []
        self.passes = 0

    def check(self, name: str, ok: bool, detail: str = "") -> None:
        if ok:
            self.passes += 1
            print(f"  [ok]   {name}" + (f" — {detail}" if detail else ""))
        else:
            self.failures.append(name)
            print(f"  [FAIL] {name}" + (f" — {detail}" if detail else ""))

    @property
    def ok(self) -> bool:
        return not self.failures


_BLEU_CASES: list[tuple[str, str]] = [
    (
        "def add(a, b):\n    return a + b",
        "def add(a, b):\n    return a + b",
    ),
    (
        "def add(a, b):\n    return a + b",
        "class Widget:\n    def render(self):\n        return None",
    ),
    (
        "def clamp(value, low, high):\n    return min(max(value, low), high)",
        "def clamp(value, low, high):\n    return max(min(value, high), low)",
    ),
    (
        "x = 1",
        "def long_function_name(alpha, beta, gamma):\n"
        "    total = alpha + beta + gamma\n"
        "    return total",
    ),
    ("", "def whatever():\n    pass"),
]


def _build_fake_dataset(repos: int = 2, functions_per_repo: int = 10) -> dict[str, list[dict[str, Any]]]:
    """构造与官方结构一致的**假数据集**（只含 python，完全离线）。

    字段名逐字对齐官方：repo/commit_sha/entrypoint_path/topic/content/dependency/
    functions/needles，needle 里含 name/start_line/end_line/path/description 等。
    """
    entries: list[dict[str, Any]] = []
    for repo_index in range(repos):
        repo_name = f"fake/repo{repo_index}"
        lines: list[str] = ['"""Fake module for repoqa selftest."""', ""]
        needles: list[dict[str, Any]] = []
        functions: dict[str, list[dict[str, Any]]] = {}

        for fn_index in range(functions_per_repo):
            name = f"solve_{repo_index}_{fn_index}"
            body = [
                f"def {name}(value, limit):",
                f'    """Fake helper {fn_index} used by the selftest."""',
                f"    total = value * {fn_index + 1}",
                "    if total > limit:",
                "        return limit",
                "    return total",
                "",
            ]
            start_line = len(lines)
            lines.extend(body)
            # 切片语义：lines[start:end]，end 不含 -> 排除最后一个空行。
            end_line = start_line + len(body) - 1
            needles.append(
                {
                    "name": name,
                    "path": "src/core.py",
                    "start_line": start_line,
                    "end_line": end_line,
                    "start_byte": start_line * 10,
                    "end_byte": end_line * 10,
                    "global_start_line": start_line,
                    "global_end_line": end_line,
                    "global_start_byte": start_line * 10,
                    "global_end_byte": end_line * 10,
                    "code_ratio": (end_line - start_line) / max(len(lines), 1),
                    "description": (
                        "Purpose: multiplies the incoming value by a constant factor.\n"
                        "Input: a numeric value and an upper limit.\n"
                        "Output: the clamped product.\n"
                        "Procedure: multiply, compare against the limit, then return."
                    ),
                }
            )
            functions.setdefault("src/core.py", []).append(
                {"name": name, "start_line": start_line, "end_line": end_line}
            )

        entries.append(
            {
                "repo": repo_name,
                "commit_sha": f"deadbeef{repo_index}",
                "entrypoint_path": "src/core.py",
                "topic": "selftest",
                "content": {"src/core.py": "\n".join(lines)},
                "dependency": {},
                "functions": functions,
                "needles": needles,
            }
        )

    return {"python": entries}


def _cmd_selftest(_args: argparse.Namespace) -> int:
    print("=== RepoQA eval selftest（离线：不需要网络 / codeseek / nltk）===")
    checker = _Checker()

    # ---------- 1) BLEU 后端 + 双实现对拍 ----------
    print(f"\n[1/6] BLEU 实现自检（当前后端：{bleu.BACKEND}）")
    comparison = bleu.compare_backends(_BLEU_CASES)
    for row in comparison["cases"]:
        builtin_score = float(row["builtin"])  # type: ignore[arg-type]
        nltk_score = row["nltk"]
        if isinstance(nltk_score, float):
            detail = f"builtin={builtin_score:.6f} nltk={nltk_score:.6f} diff={float(row['diff']):.2e}"
        else:
            detail = (
                f"builtin={builtin_score:.6f} nltk={nltk_score if nltk_score else '未安装（跳过对拍）'}"
            )
        print(f"  case{row['case']}: {detail}")

    checker.check(
        "内置 method4 给出有限值",
        all(
            isinstance(row["builtin"], float) and row["builtin"] == row["builtin"]
            for row in comparison["cases"]
        ),
    )
    checker.check(
        "完全一致的代码块 BLEU == 1.0",
        abs(bleu.sentence_bleu_method4(_BLEU_CASES[0][0], _BLEU_CASES[0][1]) - 1.0) < 1e-9,
    )
    partial = bleu.sentence_bleu_method4(_BLEU_CASES[1][0], _BLEU_CASES[1][1])
    checker.check(
        "部分重合但 2-gram 全空 → 低分非零（method4 平滑生效，与 nltk 一致）",
        0.0 < partial < 0.2,
        f"{partial:.6f}",
    )
    checker.check(
        "零 unigram 重合 → BLEU == 0.0（nltk 在平滑之前就短路返回 0）",
        bleu.sentence_bleu_method4("xyzzy plugh", "def render(self):\n    return None") == 0.0,
    )
    checker.check(
        "空候选 BLEU == 0.0（不抛 ZeroDivisionError）",
        bleu.sentence_bleu_method4("", _BLEU_CASES[4][1]) == 0.0,
    )
    if comparison["nltk_available"]:
        checker.check(
            "builtin 与 nltk 的最大绝对误差 ≤ 1e-6",
            bool(comparison["ok"]),
            f"max_abs_diff={float(comparison['max_abs_diff'] or 0.0):.3e}",
        )
    else:
        print(
            "  [info] 未安装 nltk：跳过双实现对拍，报告会标注 backend=builtin。"
            "装上 nltk 后可用同一批 raw 结果执行 `score` 复核（不改检索结果）。"
        )

    # ---------- 2) sanitize_output ----------
    print("\n[2/6] sanitize_output（官方正则）")
    raw_with_fence = "Sure, here it is:\n```python\ndef foo(x):\n    return x + 1\n```\nDone."
    sanitized = scorer.sanitize_output(raw_with_fence, "python", use_treesitter=False)
    checker.check(
        "从解释性文本中抽出第一个代码块",
        sanitized.strip().startswith("def foo(x):") and "Done." not in sanitized,
        repr(sanitized[:40]),
    )
    no_fence = "def bar():\n    return 42"
    checker.check(
        "无围栏时退回整段文本",
        scorer.sanitize_output(no_fence, "python", use_treesitter=False).strip() == no_fence,
    )
    fenced_with_lang = "```cpp\nint main() { return 0; }\n```"
    checker.check(
        "容忍 ```cpp 语言标注",
        "int main()" in scorer.sanitize_output(fenced_with_lang, "cpp", use_treesitter=False),
    )

    # ---------- 3) JSON 容错解析 ----------
    print("\n[3/6] adapter.parse_json_tolerant")
    clean = '[{"name": "foo", "file_path": "a.py"}]'
    checker.check("纯 JSON 数组", adapter.parse_json_tolerant(clean) == json.loads(clean))
    polluted = (
        "2024-06-23T10:00:00Z  INFO codeseek: loading graph\n"
        "warning: cache miss\n"
        '[{"name": "foo", "file_path": "a.py", "line_start": 1, "line_end": 3}]\n'
        "done in 1.2s\n"
    )
    parsed = adapter.parse_json_tolerant(polluted)
    checker.check(
        "日志污染后仍能截取 [..] 解析",
        isinstance(parsed, list) and parsed and parsed[0]["name"] == "foo",
    )
    checker.check("完全无 JSON 时返回 None", adapter.parse_json_tolerant("boom: not json") is None)

    # ---------- 4) 检索模式判定 + 行号切片 ----------
    print("\n[4/6] detect_mode / build_output_block")
    checker.check(
        "含 code_block → hybrid",
        adapter.detect_mode([{"code_block": "x"}]) == adapter.MODE_HYBRID,
    )
    checker.check(
        "无 code_block → name_fallback",
        adapter.detect_mode([{"name": "foo"}]) == adapter.MODE_FALLBACK,
    )
    checker.check("空结果 → unknown_empty", adapter.detect_mode([]) == adapter.MODE_UNKNOWN)

    with tempfile.TemporaryDirectory(prefix="repoqa-selftest-") as tmp:
        tmp_path = Path(tmp)
        fake_dir = tmp_path / "repos" / "python" / "fake__repo0"
        dataset = _build_fake_dataset()
        ok, detail = data.materialize_repo(dataset["python"][0], fake_dir, git_init=False)
        checker.check("materialize_repo 落盘成功", ok, detail)
        checker.check("落盘文件内容可读回", (fake_dir / "src" / "core.py").is_file())

        # 路径穿越必须被拒绝
        traversal_entry = {
            "repo": "evil/repo",
            "commit_sha": "x",
            "content": {"../escape.py": "print('nope')"},
        }
        try:
            data.materialize_repo(traversal_entry, tmp_path / "evil", git_init=False)
            traversal_blocked = False
        except ValueError:
            traversal_blocked = True
        checker.check("拒绝 .. 路径穿越", traversal_blocked)

        # 1-based 行号切片
        repo_file = fake_dir / "src" / "core.py"
        total_lines = len(repo_file.read_text(encoding="utf-8").split("\n"))
        item = {"file_path": "src/core.py", "line_start": 3, "line_end": 8}
        block = adapter.build_output_block(item, fake_dir, "python")
        checker.check(
            "按 1-based 行号切片并加围栏",
            block.startswith("```python") and "def solve_0_0" in block,
            f"文件共 {total_lines} 行",
        )
        checker.check(
            "越界路径被拒绝（file_path 指向仓库外）→ 空串",
            adapter.build_output_block(
                {"file_path": "/etc/passwd", "line_start": 1, "line_end": 2}, fake_dir, "python"
            )
            == "",
        )

        # ---------- 5) needle 切片 + scorer 判定 ----------
        print("\n[5/6] scorer 判定（10-needle 候选集 + threshold）")
        tasks = data.select_subset(dataset, ["python"], repos_per_language=2)
        checker.check("子集题目数 = 2 repo × 10 needle", len(tasks) == 20, f"实际 {len(tasks)}")

        repo_entry = dataset["python"][0]
        needle0 = repo_entry["needles"][0]
        ground_truth = data.needle_text(
            repo_entry["content"][needle0["path"]], needle0["start_line"], needle0["end_line"]
        )
        checker.check("needle 切片非空且含函数名", ground_truth.startswith("def solve_0_0"))

        perfect = scorer.score_one(
            tasks[0], [f"```python\n{ground_truth}\n```"], repo_entry, threshold=0.8
        )
        checker.check(
            "输出 == ground truth → pass，sim≈1.0",
            perfect["passed"] and perfect["best_similarity"] > 0.999,
            f"sim={perfect['best_similarity']:.4f} target={perfect['best_target']}",
        )

        noise = " ".join(f"junk{i}" for i in range(60))
        bad = scorer.score_one(
            tasks[0], [f"```python\n{noise}\n```"], repo_entry, threshold=0.8
        )
        checker.check("无关输出 → fail", not bad["passed"], f"sim={bad['best_similarity']:.4f}")
        checker.check(
            "空输出 → fail（reason=empty_output）",
            not scorer.score_one(tasks[0], [], repo_entry)["passed"],
        )

        # ---------- 6) raw → score → report 全链路 ----------
        print("\n[6/6] runner.score_only → report.json / report.md 全链路")
        workdir = tmp_path / "workdir"
        raw_dir = workdir / data.RAW_DIR
        raw_dir.mkdir(parents=True)
        data.write_tasks(tasks, workdir / data.TASKS_FILE)

        for index, task in enumerate(tasks):
            entry = dataset[task.language][task.repo_index]
            needle = entry["needles"][task.needle_index]
            output = data.needle_text(
                entry["content"][needle["path"]], needle["start_line"], needle["end_line"]
            )
            if index % 5 == 4:
                output = " ".join(f"junk{i}" for i in range(40))  # 故意制造失败案例
            payload = {
                "task_id": task.id,
                "language": task.language,
                "repo": task.repo,
                "needle_name": task.needle_name,
                "query": task.description,
                "mode": adapter.MODE_HYBRID if index % 2 == 0 else adapter.MODE_FALLBACK,
                "results_count": 3,
                "outputs": [f"```python\n{output}\n```"],
                "results_meta": [{"symbol": task.needle_name, "file_path": "src/core.py"}],
                "error": "",
                "elapsed_s": 0.01,
                "retrieved_at": "selftest",
            }
            (raw_dir / f"{task.id}.json").write_text(
                json.dumps(payload, ensure_ascii=False), encoding="utf-8"
            )

        summary = runner.score_only(tasks=tasks, dataset=dataset, workdir=workdir, threshold=0.8)
        report = summary.report
        expected_passed = sum(1 for index in range(len(tasks)) if index % 5 != 4)
        checker.check(
            "aggregate 通过数符合预期",
            report["aggregate"]["all"]["passed"] == expected_passed,
            f"{report['aggregate']['all']['passed']}/{len(tasks)}",
        )
        checker.check(
            "name_fallback 触发醒目的检索模式警示",
            any("name_fallback" in warning for warning in report["warnings"]),
        )
        checker.check(
            "失败案例 Top 10 非空且 ≤ 10",
            0 < len(report["failure_top"]) <= 10,
            f"{len(report['failure_top'])} 条",
        )
        json_path = Path(summary.report_json_path)
        md_path = Path(summary.report_md_path)
        checker.check("report.json 可被 json.loads", isinstance(json.loads(json_path.read_text(encoding="utf-8")), dict))
        markdown = md_path.read_text(encoding="utf-8")
        checker.check(
            "report.md 含 5 个规定小节",
            all(
                heading in markdown
                for heading in (
                    "## 1. Summary",
                    "## 2. Retrieval Mode Statistics",
                    "## 3. Breakdown by Language × Repo",
                    "## 4. Top 10 Failures (sorted by best_similarity, descending)",
                    "## 5. Reproduction Commands",
                )
            ),
        )
        checker.check(
            "accuracy 保留 1 位小数",
            report["aggregate"]["all"]["accuracy_percent"].count(".") == 1
            and len(report["aggregate"]["all"]["accuracy_percent"].split(".")[-1].rstrip("%")) == 1,
            report["aggregate"]["all"]["accuracy_percent"],
        )
        print("\n  --- report.md 前 18 行（节选）---")
        for line in markdown.splitlines()[:18]:
            print(f"  | {line}")

    print("\n=== 汇总 ===")
    print(f"通过 {checker.passes} 项，失败 {len(checker.failures)} 项")
    if checker.failures:
        for name in checker.failures:
            print(f"  - FAILED: {name}")
        return 1
    print("全部自检通过（离线，未使用网络 / codeseek 二进制）")
    return 0


# ---------------------------------------------------------------------------
# argparse
# ---------------------------------------------------------------------------


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="python -m repoqa_eval",
        description="用 RepoQA 基准评测 codeseek（BLEU-4 method4 + 10-needle 候选集 + threshold 0.8）",
    )
    sub = parser.add_subparsers(dest="command", required=True)

    prepare = sub.add_parser("prepare", help="下载数据 + 抽子集 + 落盘仓库快照 + 写 tasks.jsonl")
    prepare.add_argument("--workdir", default=DEFAULT_WORKDIR, help=f"工作目录（默认 {DEFAULT_WORKDIR}）")
    prepare.add_argument(
        "--languages",
        type=_parse_languages,
        default=None,
        help=f"逗号分隔语言，默认 {','.join(data.DEFAULT_LANGUAGES)}（不含 go）",
    )
    prepare.add_argument("--repos-per-language", type=int, default=2, help="每语言取前 N 个 repo（默认 2）")
    prepare.add_argument("--proxy", default=None, help="下载代理，如 http://127.0.0.1:7897")
    prepare.add_argument("--force", action="store_true", help="强制重写仓库快照")
    prepare.set_defaults(func=cmd_prepare)

    run_parser = sub.add_parser("run", help="执行评测并产出 report.json / report.md")
    run_parser.add_argument("--workdir", default=DEFAULT_WORKDIR)
    run_parser.add_argument("--codeseek-bin", default=None, help="codeseek 二进制路径（默认查 CODESEEK_BIN / rust-core/target/release）")
    run_parser.add_argument("--repo-root", default=None, help="仓库根（用于定位二进制，默认自动推断）")
    run_parser.add_argument("--limit", type=int, default=5, help="codeseek search --limit（默认 5）")
    run_parser.add_argument("--limit-questions", type=int, default=0, help="只跑前 N 题（冒烟，0=全部）")
    run_parser.add_argument("--threshold", type=float, default=scorer.DEFAULT_THRESHOLD, help="判定阈值（默认 0.8）")
    run_parser.add_argument("--concurrency", type=int, default=4, help="search 并发线程数（默认 4）")
    run_parser.add_argument("--timeout", type=float, default=180.0, help="单题 search 超时秒数（默认 180）")
    run_parser.add_argument("--init-timeout", type=float, default=1800.0, help="单仓库 init 超时秒数（默认 1800）")
    run_parser.add_argument("--force", action="store_true", help="忽略已有 raw 结果，强制重跑检索")
    run_parser.add_argument("--auto-build", action="store_true", help="找不到二进制时自动 cargo build --release")
    run_parser.add_argument("--require-embedding", action="store_true", help="未配置 embedding.api_token 时直接退出")
    run_parser.add_argument("--quiet", action="store_true", help="不打印每题进度")
    run_parser.set_defaults(func=cmd_run)

    score = sub.add_parser("score", help="只用已有 raw 结果重新判分（不重跑检索）")
    score.add_argument("--workdir", default=DEFAULT_WORKDIR)
    score.add_argument("--threshold", type=float, default=scorer.DEFAULT_THRESHOLD)
    score.add_argument("--limit-questions", type=int, default=0)
    score.add_argument("--limit", type=int, default=5, help="仅用于报告里的复现命令展示")
    score.add_argument("--codeseek-bin", default=None, help="可选：仅用于在报告里记录二进制指纹")
    score.add_argument("--repo-root", default=None)
    score.set_defaults(func=cmd_score)

    selftest = sub.add_parser("selftest", help="离线自检：BLEU 实现 + sanitize + 解析 + scorer/report 全链路")
    selftest.set_defaults(func=_cmd_selftest)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    return int(args.func(args))
