"""官方判定逻辑复刻：sanitize_output + 10-needle 候选集 + threshold。

官方 ``repoqa/compute_score.py`` 的核心（逐字语义）：

.. code-block:: text

    sanitized = sanitize_output(model_output)             # 只取 output[0]
    best_similarity, best_target = -inf, None
    for needle in repo["needles"]:                        # 候选集 = 该 repo 的 10 条 needle
        cand = "\\n".join(content[needle.path].split("\\n")[needle.start_line:needle.end_line])
        sim = compute_function_similarity(sanitized, cand)
        if sim > best_similarity:
            best_similarity, best_target = sim, needle["name"]
    passed = (best_target == ground_truth_name) and (best_similarity >= threshold)   # 默认 0.8

注意两个容易写错的细节：

1. 候选集**只有 10 条 needle**，不是全仓库函数——比"全仓库检索"宽松得多。
2. 命中判定要求"BLEU 最高者恰好是 ground truth"，而不是"ground truth 的 BLEU ≥ 0.8"。
   所以一个近乎正确的答案也可能因为另一条 needle 更像而判错（该行为与官方一致）。
"""

from __future__ import annotations

import re
from typing import Any, Iterable, Sequence

from . import bleu
from .data import Task, needle_text

__all__ = [
    "DEFAULT_THRESHOLD",
    "CODE_BLOCK_PATTERN",
    "score_one",
    "score_tasks",
    "sanitize_output",
    "treesitter_available",
]

#: 官方默认阈值。
DEFAULT_THRESHOLD = 0.8

#: 官方正则（逐字）：抓第一个围栏代码块，容忍 ```python 这类语言标注。
CODE_BLOCK_PATTERN = re.compile(r"^```(?:\w+)?\s*\n(.*?)(?=^```)```", re.DOTALL | re.MULTILINE)

#: 各语言的"函数定义"AST 节点名（仅用于可选 tree-sitter 路径）。
_FUNCTION_NODE_NAMES: dict[str, set[str]] = {
    "python": {"function_definition"},
    "java": {"method_declaration", "constructor_declaration"},
    "cpp": {"function_definition"},
    "rust": {"function_item"},
    "typescript": {
        "function_declaration",
        "method_definition",
        "function_expression",
        "arrow_function",
        "generator_function_declaration",
    },
    "go": {"function_declaration", "method_declaration"},
}


def treesitter_available() -> bool:
    """tree-sitter 为**可选**依赖：只要能 import 解析器工厂就返回 True。"""
    try:  # pragma: no cover - 取决于运行环境
        from tree_sitter_language_pack import get_parser  # noqa: F401
    except Exception:
        return False
    return True


def _extract_first_function_with_treesitter(code: str, language: str) -> str | None:
    """用 tree-sitter 从代码块中取**第一个函数定义**的源码；不可用时返回 None。

    官方用 tree-sitter query 做这件事；这里退化为"按节点名找第一个命中节点"，
    语义等价（取第一个函数定义），但不需要维护 query 字符串。
    """
    if not code.strip():
        return None
    node_names = _FUNCTION_NODE_NAMES.get(language)
    if not node_names:
        return None
    try:  # pragma: no cover - 取决于运行环境
        from tree_sitter_language_pack import get_parser

        parser = get_parser(language)
        tree = parser.parse(code.encode("utf-8"))
    except Exception:
        return None

    stack = [tree.root_node]
    while stack:
        node = stack.pop(0)
        if node.type in node_names:
            try:
                return code.encode("utf-8")[node.start_byte : node.end_byte].decode(
                    "utf-8", errors="replace"
                )
            except Exception:
                return None
        # 广度优先：先按源码顺序展开子节点（tree-sitter 子节点天然按位置排序）。
        stack = list(node.children) + stack
    return None


def sanitize_output(raw: str, language: str, use_treesitter: bool = True) -> str:
    """复刻官方 ``sanitize_output``。

    1. 用官方正则从模型输出里抓第一个围栏代码块；抓不到就用整段输出（官方行为）；
    2. 若装了 tree-sitter，再取块内第一个函数定义；否则原样返回（**不报错**）。

    :param raw: 被测系统原始输出（本套件里是 codeseek 检索结果拼出的 fenced block）。
    :param language: ``python`` / ``cpp`` / ``java`` / ``typescript`` / ``rust`` / ``go``。
    :param use_treesitter: 便于 selftest 强制走"无 tree-sitter"路径。
    """
    text = raw or ""
    match = CODE_BLOCK_PATTERN.search(text)
    block = match.group(1) if match else text

    if use_treesitter:
        extracted = _extract_first_function_with_treesitter(block, language)
        if extracted:
            return extracted

    return block.strip("\n")


def _needle_source(repo_entry: dict[str, Any], needle: dict[str, Any]) -> str:
    """取某条 needle 的参考代码；路径缺失/越界时返回空串（该候选视为 0 分）。"""
    path = needle.get("path")
    content = repo_entry.get("content", {})
    if not isinstance(path, str) or path not in content:
        return ""
    return needle_text(content[path], int(needle["start_line"]), int(needle["end_line"]))


def score_one(
    task: Task,
    codeseek_outputs: Sequence[str],
    repo_entry: dict[str, Any],
    threshold: float = DEFAULT_THRESHOLD,
    use_treesitter: bool = True,
) -> dict[str, Any]:
    """对单题判分（完全复刻官方流程）。

    :param codeseek_outputs: 被测系统的输出候选列表；**只用第 0 个**（官方 ``output[0]``）。
    :param repo_entry: 该 repo 在数据集中的原始条目（提供 ``content`` 与 ``needles``）。
    :return: ``{passed, best_similarity, best_target, is_best_similar, sanitized_len, ...}``
    """
    needles: list[dict[str, Any]] = repo_entry.get("needles", [])

    if not codeseek_outputs or not (codeseek_outputs[0] or "").strip():
        return {
            "passed": False,
            "best_similarity": 0.0,
            "best_target": None,
            "is_best_similar": False,
            "sanitized_len": 0,
            "candidates": len(needles),
            "reason": "empty_output",
        }

    sanitized = sanitize_output(codeseek_outputs[0], task.language, use_treesitter=use_treesitter)
    if not sanitized.strip():
        return {
            "passed": False,
            "best_similarity": 0.0,
            "best_target": None,
            "is_best_similar": False,
            "sanitized_len": 0,
            "candidates": len(needles),
            "reason": "empty_sanitized",
        }

    best_similarity = -1.0
    best_target: str | None = None
    best_index = -1
    # 与官方一致：候选集就是该 repo 的 needle 列表本身，顺序即数据顺序，
    # 且严格用 ``>`` 比较（并列时保留先出现的那条）。
    for index, needle in enumerate(needles):
        candidate = _needle_source(repo_entry, needle)
        similarity = bleu.sentence_bleu_method4(sanitized, candidate)
        if similarity > best_similarity:
            best_similarity = similarity
            best_target = needle.get("name")
            best_index = index

    is_best_similar = best_target == task.needle_name
    passed = bool(is_best_similar and best_similarity >= threshold)

    return {
        "passed": passed,
        "best_similarity": max(best_similarity, 0.0),
        "best_target": best_target,
        "is_best_similar": is_best_similar,
        "sanitized_len": len(sanitized),
        "candidates": len(needles),
        "best_index": best_index,
        "reason": "ok" if passed else ("wrong_target" if not is_best_similar else "below_threshold"),
    }


def score_tasks(
    tasks: Iterable[Task],
    outputs_by_task_id: dict[str, list[str]],
    dataset: dict[str, list[dict[str, Any]]],
    threshold: float = DEFAULT_THRESHOLD,
    use_treesitter: bool = True,
) -> dict[str, Any]:
    """批量判分并聚合 ``all`` / ``by_language`` 的 accuracy。

    :return: ``{"threshold", "bleu_backend", "aggregate", "per_task"}``
    """
    per_task: list[dict[str, Any]] = []
    for task in tasks:
        repo_entry = dataset[task.language][task.repo_index]
        result = score_one(
            task,
            outputs_by_task_id.get(task.id, []),
            repo_entry,
            threshold=threshold,
            use_treesitter=use_treesitter,
        )
        per_task.append(
            {
                "id": task.id,
                "language": task.language,
                "repo": task.repo,
                "needle_name": task.needle_name,
                "needle_path": task.needle_path,
                "start_line": task.start_line,
                "end_line": task.end_line,
                "position_ratio": task.position_ratio,
                **result,
            }
        )

    return {
        "threshold": threshold,
        "bleu_backend": bleu.BACKEND,
        "aggregate": {
            "all": _aggregate(per_task),
            "by_language": {
                language: _aggregate([row for row in per_task if row["language"] == language])
                for language in _ordered_languages(per_task)
            },
        },
        "per_task": per_task,
    }


def _ordered_languages(rows: Sequence[dict[str, Any]]) -> list[str]:
    """按首次出现顺序返回语言列表（保持数据集顺序，输出稳定可 diff）。"""
    seen: list[str] = []
    for row in rows:
        if row["language"] not in seen:
            seen.append(row["language"])
    return seen


def _aggregate(rows: Sequence[dict[str, Any]]) -> dict[str, Any]:
    """算一组题的通过率。``accuracy`` 保留 4 位小数供内部使用，展示层再取 1 位。"""
    total = len(rows)
    passed = sum(1 for row in rows if row.get("passed"))
    return {
        "total": total,
        "passed": passed,
        "accuracy": (passed / total) if total else 0.0,
    }
