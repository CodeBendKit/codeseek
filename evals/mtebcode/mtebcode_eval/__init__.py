"""MTEB(Code, v1) × codeseek 端到端评测套件。

模块划分（数据流见 ``README.md`` 的链路图）：

    data.py       datasets-server REST 客户端 → 缓存/重试 → 任务注册表 → 池化抽样 → 语料物化
                  → tasks.jsonl + meta.json（``<workdir>/corpora/<task_slug>/`` + git init）
    adapter.py    codeseek CLI 调用：二进制定位 / init / search / 容错解析 / 结果 → doc_id 反查
    metric.py     nDCG@k / recall@k / MRR@k / precision@k（纯标准库，无 numpy）
    scorer.py     检索结果 → doc_id 排序 → 指标 → 任务级 & Mean(Task) 聚合
    runner.py     编排：串行 init + 并发 search + 断点续跑 + 评分 + 报告
    report.py     report.json / report.md
    selftest.py   完全离线的自检（数值 + 解析 + 反查 + 端到端报告）
    cli.py        argparse 入口（prepare / run / score / selftest）
"""

from __future__ import annotations

__version__ = "1.0.0"

__all__ = ["__version__"]
