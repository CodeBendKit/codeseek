"""RepoQA × codeseek 端到端评测套件。

模块划分（数据流见 README.md 的链路图）：

    data.py     数据集下载/缓存 → 子集抽样 → 仓库快照落盘 → tasks.jsonl
    bleu.py     BLEU（nltk 优先，内置 method4 复刻实现兜底）
    scorer.py   官方判定逻辑：sanitize + 10-needle 候选集 + threshold
    adapter.py  codeseek CLI 调用：build 检测 / init / search / 容错解析 / 输出组装
    runner.py   编排：串行 init + 并发 search + 断点续跑 + 进度
    report.py   report.json / report.md
    cli.py      argparse 入口（prepare / run / score / selftest）
"""

from __future__ import annotations

__version__ = "1.0.0"

__all__ = ["__version__"]
