"""BLEU 计分 —— 逐字复刻 RepoQA 官方口径（nltk sentence_bleu + SmoothingFunction().method4）。

官方 ``repoqa/metric.py`` 的相似度实现（逐字）：

.. code-block:: python

    candidate_tokens = [item for item in re.split(r"\\s+", candidate_function.strip())]
    reference_tokens = [item for item in re.split(r"\\s+", reference_function.strip())]
    chencherry = SmoothingFunction()          # nltk.translate.bleu_score
    return sentence_bleu([reference_tokens], candidate_tokens, smoothing_function=chencherry.method4)

也就是说：**按空白切词**（不是 AST token）、默认权重 ``(0.25, 0.25, 0.25, 0.25)``（BLEU-4）、
Chen & Cherry ``method4`` 平滑。

本模块优先调用 nltk；nltk 缺失时退化为内置复刻实现 ``builtin_sentence_bleu_method4``。
内置实现对照 nltk 3.10.4 的 ``corpus_bleu`` / ``modified_precision`` / ``brevity_penalty`` /
``SmoothingFunction.method4`` 逐行复刻，关键差异点都在注释里标了依据。
"""

from __future__ import annotations

import math
import re
from collections import Counter
from typing import Iterable, Sequence

__all__ = [
    "BACKEND",
    "METHOD4_K",
    "WEIGHTS",
    "builtin_sentence_bleu_method4",
    "sentence_bleu_method4",
    "tokenize",
    "ngrams",
]

#: nltk 的 ``SmoothingFunction.__init__(epsilon=0.1, alpha=5, k=5)`` —— method4 用到 ``k``。
METHOD4_K = 5

#: nltk ``sentence_bleu`` 的默认权重：BLEU-4，四阶均匀。
WEIGHTS: tuple[float, float, float, float] = (0.25, 0.25, 0.25, 0.25)

_MAX_N = 4


def _detect_backend() -> str:
    """探测 nltk 是否可用，返回 ``"nltk"`` 或 ``"builtin"``。"""
    try:  # pragma: no cover - 取决于运行环境
        from nltk.translate.bleu_score import SmoothingFunction, sentence_bleu  # noqa: F401
    except Exception:
        return "builtin"
    return "nltk"


#: 实际使用的 BLEU 后端（进程启动时探测一次）。报告里必须写明用了哪个。
BACKEND: str = _detect_backend()


def tokenize(text: str) -> list[str]:
    """官方切词：``re.split(r"\\s+", text.strip())``。

    注意：**故意不丢弃空串**。对空输入 ``""``，``re.split`` 返回 ``[""]``（长度 1 的
    "token"），nltk 端同样是这个结果；过滤空串会改变未匹配时的 BLEU 数值，因此保持原样。
    """
    return [item for item in re.split(r"\s+", text.strip())]


def ngrams(tokens: Sequence[str], n: int) -> Iterable[tuple[str, ...]]:
    """生成 n-gram（与 ``nltk.util.ngrams`` 一致：长度为 n 的滑窗元组）。"""
    for i in range(len(tokens) - n + 1):
        yield tuple(tokens[i : i + n])


def _modified_precision(
    reference_tokens: Sequence[str], hypothesis_tokens: Sequence[str], n: int
) -> tuple[int, int]:
    """复刻 ``nltk.translate.bleu_score.modified_precision``（单参考句情形）。

    返回 ``(numerator, denominator)``：

    * ``numerator``   = clipped 匹配数（每个 hyp n-gram 的计数被参考句同 n-gram 计数截断）；
    * ``denominator`` = ``max(1, hyp 的 n-gram 总数)``
      —— nltk 用 ``max(1, ...)`` 避免 n 大于句长时除零。

    依据：nltk 3.10.4 ``bleu_score.py:285`` 起（``Counter(ngrams(...))`` + ``max_counts`` +
    ``clipped_counts`` + ``denominator = max(1, sum(counts.values()))``）。
    """
    hyp_counts: Counter[tuple[str, ...]]
    if len(hypothesis_tokens) >= n:
        hyp_counts = Counter(ngrams(hypothesis_tokens, n))
    else:
        hyp_counts = Counter()

    ref_counts: Counter[tuple[str, ...]]
    if len(reference_tokens) >= n:
        ref_counts = Counter(ngrams(reference_tokens, n))
    else:
        ref_counts = Counter()

    numerator = 0
    for gram, count in hyp_counts.items():
        numerator += min(count, ref_counts.get(gram, 0))

    denominator = max(1, sum(hyp_counts.values()))
    return numerator, denominator


def _brevity_penalty(closest_ref_len: int, hyp_len: int) -> float:
    """复刻 ``nltk.translate.bleu_score.brevity_penalty``。

    * ``hyp_len > ref_len`` → 1（不惩罚）；
    * ``hyp_len == 0``     → 0（空候选 BLEU 记 0，避免除零）；
    * 其余                 → ``exp(1 - ref_len / hyp_len)``。
    """
    if hyp_len > closest_ref_len:
        return 1.0
    if hyp_len == 0:
        return 0.0
    return math.exp(1 - closest_ref_len / hyp_len)


def builtin_sentence_bleu_method4(candidate: str, reference: str) -> float:
    """不依赖 nltk 的 BLEU-4 + method4 平滑（单参考句）。

    逐段对齐 nltk 3.10.4 的 ``corpus_bleu``：

    1. 对 n=1..4 求 modified precision ``(num, den)``，``den`` 已 ``max(1, ...)``；
    2. **先判短路**：``p_numerators[1] == 0`` 时直接返回 0（nltk 在平滑之前就返回，
       所以"一个 unigram 都没命中"必然是 0 分，平滑救不回来）；
    3. method4 平滑：``incvnt`` 从 1 开始、只在真正替换时自增；
       替换值为 ``(1 / (2**incvnt * K / log(hyp_len))) / den``，
       且仅在 ``p_i.numerator == 0 and hyp_len > 1`` 时生效；
    4. ``bp * exp(fsum(w_i * log(p_i) for ... if p_i > 0))``
       —— 精度为 0 的阶会被 nltk 的生成器过滤掉（等价于该阶不参与加权）。
    """
    hypothesis_tokens = tokenize(candidate)
    reference_tokens = tokenize(reference)

    hyp_len = len(hypothesis_tokens)
    ref_len = len(reference_tokens)

    # 单参考句：closest_ref_length 就是参考句长度本身。
    bp = _brevity_penalty(ref_len, hyp_len)

    precisions: list[tuple[int, int]] = []
    for n in range(1, _MAX_N + 1):
        precisions.append(_modified_precision(reference_tokens, hypothesis_tokens, n))

    # 与 nltk 一致：unigram 零命中直接返回 0（早于平滑）。
    if precisions[0][0] == 0:
        return 0.0

    # nltk 传给 method4 的 hyp_len 实为"语料级 hyp_lengths"，对单句即 len(hypothesis)。
    smooth_hyp_len = hyp_len if hyp_len else len(hypothesis_tokens)

    smoothed: list[float] = []
    incvnt = 1
    for numerator, denominator in precisions:
        if numerator == 0 and smooth_hyp_len > 1:
            # 依据：nltk SmoothingFunction.method4 —— numerator 变量名沿用官方源码。
            smoothed_numerator = 1 / (2**incvnt * METHOD4_K / math.log(smooth_hyp_len))
            smoothed.append(smoothed_numerator / denominator)
            incvnt += 1
        else:
            smoothed.append(numerator / denominator)

    terms = [
        weight * math.log(probability)
        for weight, probability in zip(WEIGHTS, smoothed)
        if probability > 0
    ]
    # math.fsum 与 nltk 的求和方式一致（精确求和后一次性取 exp）。
    return bp * math.exp(math.fsum(terms)) if terms else 0.0


def sentence_bleu_method4(candidate: str, reference: str) -> float:
    """官方口径的句级 BLEU（nltk 优先，缺失时用内置复刻）。

    :param candidate: 被测系统给出的函数代码（已 sanitize）。
    :param reference: ground-truth needle 的函数代码。
    :return: ``[0, 1]`` 之间的相似度；任何异常都退化为 0.0，避免单题崩溃影响整轮评测。
    """
    candidate_tokens = tokenize(candidate)
    reference_tokens = tokenize(reference)

    if BACKEND == "nltk":
        try:
            from nltk.translate.bleu_score import SmoothingFunction, sentence_bleu

            chencherry = SmoothingFunction()
            return float(
                sentence_bleu(
                    [reference_tokens],
                    candidate_tokens,
                    smoothing_function=chencherry.method4,
                )
            )
        except Exception:
            # nltk 版本异常 / 空输入触发 ZeroDivisionError 等：退回内置实现。
            return builtin_sentence_bleu_method4(candidate, reference)

    return builtin_sentence_bleu_method4(candidate, reference)


def compare_backends(
    cases: Sequence[tuple[str, str]], tolerance: float = 1e-6
) -> dict[str, object]:
    """把内置实现与 nltk 在一组样例上做逐例比对（selftest 用）。

    :return: ``{"backend": 当前后端, "nltk_available": bool, "max_abs_diff": float,
              "cases": [{"name", "builtin", "nltk", "diff"}...], "ok": bool}``

    nltk 不可用时 ``max_abs_diff`` 为 ``None``，``ok`` 仅取决于内置实现是否给出有限值。
    """
    rows: list[dict[str, object]] = []
    nltk_available = BACKEND == "nltk"
    max_diff = 0.0

    for index, (candidate, reference) in enumerate(cases):
        builtin_score = builtin_sentence_bleu_method4(candidate, reference)
        row: dict[str, object] = {
            "case": index,
            "builtin": builtin_score,
            "nltk": None,
            "diff": None,
        }
        if nltk_available:
            try:
                from nltk.translate.bleu_score import SmoothingFunction, sentence_bleu

                chencherry = SmoothingFunction()
                nltk_score = float(
                    sentence_bleu(
                        [tokenize(reference)],
                        tokenize(candidate),
                        smoothing_function=chencherry.method4,
                    )
                )
                diff = abs(builtin_score - nltk_score)
                max_diff = max(max_diff, diff)
                row["nltk"] = nltk_score
                row["diff"] = diff
            except Exception as exc:  # nltk 自身抛错（例如空候选除零）
                row["nltk"] = f"<error: {type(exc).__name__}: {exc}>"
        rows.append(row)

    ok = all(
        isinstance(row["builtin"], float) and math.isfinite(row["builtin"]) for row in rows
    )
    if nltk_available:
        ok = ok and max_diff <= tolerance

    return {
        "backend": BACKEND,
        "nltk_available": nltk_available,
        "max_abs_diff": max_diff if nltk_available else None,
        "tolerance": tolerance,
        "cases": rows,
        "ok": ok,
    }
