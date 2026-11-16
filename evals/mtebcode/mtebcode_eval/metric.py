"""排序指标：nDCG@k / recall@k / MRR@k / precision@k（**纯标准库，不依赖 numpy**）。

口径与 BeIR / ``pytrec_eval`` 保持一致：

.. code-block:: text

    DCG@k   = Σ_{i=0}^{k-1} (2^{rel_i} - 1) / log2(i + 2)
    IDCG@k  = DCG@k with the ideal (descending) ordering of the graded qrels
    nDCG@k  = DCG@k / IDCG@k         (0.0 when IDCG == 0)

约定：

* ``qrels`` 是 ``{doc_id: relevance}``，relevance 为 ``>0`` 的浮点数（MTEB / CoIR 的
  ``score`` 通常就全是 1，此时 nDCG 退化成二值情形，但实现按分级相关处理）；
* **没有正例 / qrels 为空的 query 一律返回 ``None``**，由调用方跳过 —— 不把 ``0.0``
  混进均值里拉低分数（MTEB 的聚合同样跳过没有正例的 query）；
* ``ranked_doc_ids`` 中的重复项应当由调用方先去重（:mod:`mtebcode_eval.adapter`
  的 ``resolve_doc_ids`` 已经去重保序）。
"""

from __future__ import annotations

import math
from typing import Any, Mapping, Sequence

__all__ = [
    "dcg",
    "mrr_at_k",
    "ndcg_at_k",
    "precision_at_k",
    "recall_at_k",
    "relevant_docs",
]


def relevant_docs(qrels: Mapping[str, float] | None) -> dict[str, float]:
    """只保留 ``relevance > 0`` 的 qrels（0/负数视为不相关）。"""
    if not qrels:
        return {}
    result: dict[str, float] = {}
    for doc_id, score in qrels.items():
        try:
            value = float(score)
        except (TypeError, ValueError):
            continue
        if value > 0:
            result[str(doc_id)] = value
    return result


def dcg(gains: Sequence[float], k: int) -> float:
    """按预测顺序累加折损增益（``gains`` 已按排名排列）。"""
    if k <= 0:
        return 0.0
    total = 0.0
    for index, gain in enumerate(gains[:k]):
        total += (2.0**float(gain) - 1.0) / math.log2(index + 2)
    return total


def ndcg_at_k(ranked_doc_ids: Sequence[str], qrels: Mapping[str, float], k: int = 10) -> float | None:
    """nDCG@k；无正例时返回 ``None``。"""
    graded = relevant_docs(qrels)
    if not graded:
        return None
    gains = [graded.get(str(doc_id), 0.0) for doc_id in ranked_doc_ids]
    ideal = sorted(graded.values(), reverse=True)
    idcg = dcg(ideal, k)
    if idcg <= 0:
        return None
    return dcg(gains, k) / idcg


def recall_at_k(ranked_doc_ids: Sequence[str], qrels: Mapping[str, float], k: int = 10) -> float | None:
    """recall@k = |Top-k ∩ relevant| / |relevant|；无正例时返回 ``None``。"""
    graded = relevant_docs(qrels)
    if not graded:
        return None
    top = {str(doc_id) for doc_id in ranked_doc_ids[:k]}
    hit = len(top & set(graded))
    return hit / len(graded)


def mrr_at_k(ranked_doc_ids: Sequence[str], qrels: Mapping[str, float], k: int = 10) -> float | None:
    """MRR@k = 1 / rank of the first relevant doc（未命中为 0.0）；无正例时返回 ``None``。"""
    graded = relevant_docs(qrels)
    if not graded:
        return None
    for index, doc_id in enumerate(ranked_doc_ids[:k]):
        if str(doc_id) in graded:
            return 1.0 / (index + 1)
    return 0.0


def precision_at_k(ranked_doc_ids: Sequence[str], qrels: Mapping[str, float], k: int = 10) -> float | None:
    """precision@k = |Top-k ∩ relevant| / k；无正例时返回 ``None``。"""
    graded = relevant_docs(qrels)
    if not graded or k <= 0:
        return None
    top = [str(doc_id) for doc_id in ranked_doc_ids[:k]]
    if not top:
        return 0.0
    return len([doc_id for doc_id in top if doc_id in graded]) / float(k)


def mean(values: Sequence[float | None]) -> float | None:
    """均值；自动跳过 ``None``（``None`` 与空列表都返回 ``None``）。"""
    picked = [float(value) for value in values if value is not None]
    if not picked:
        return None
    return sum(picked) / len(picked)


def summarize(values: Sequence[float | None]) -> dict[str, Any]:
    """``{mean, n, skipped}`` —— 报告里统一用它描述一个指标的分布。"""
    picked = [float(value) for value in values if value is not None]
    return {
        "mean": (sum(picked) / len(picked)) if picked else None,
        "n": len(picked),
        "skipped": len(values) - len(picked),
    }
