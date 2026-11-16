"""离线自检：nDCG 数值 / 容错解析 / doc_id 反查 / 端到端报告。

设计原则（与 repoqa 的 ``selftest`` 一致）：**完全离线**，不联网、不需要 codeseek 二进制、
不需要 numpy/nltk —— 只验证本套件的纯逻辑。断言失败会打印 `expected` / `actual` 差异，
返回码 1；全部通过打印 ``SELFTEST OK`` 并返回 0。
"""

from __future__ import annotations

import json
import math
import tempfile
from pathlib import Path
from typing import Any

from . import adapter, data, metric, report as report_module, runner, scorer

__all__ = ["run_selftest"]


class _Checker:
    """收集自检结果（不依赖 pytest）。"""

    def __init__(self) -> None:
        self.failures: list[tuple[str, str]] = []
        self.passes = 0

    def check(self, name: str, ok: bool, detail: str = "") -> None:
        if ok:
            self.passes += 1
            print(f"  [ok]   {name}" + (f" — {detail}" if detail else ""))
        else:
            self.failures.append((name, detail))
            print(f"  [FAIL] {name}" + (f" — {detail}" if detail else ""))

    def check_close(self, name: str, expected: float, actual: float | None, tol: float = 1e-9) -> None:
        if actual is None:
            self.check(name, False, f"expected {expected!r}, actual None")
            return
        diff = abs(float(actual) - float(expected))
        self.check(name, diff < tol, f"expected {expected:.12f}, actual {float(actual):.12f}, diff {diff:.2e}")

    @property
    def ok(self) -> bool:
        return not self.failures


def _section(title: str) -> None:
    print(f"\n{title}")


def _raises_value_error(func) -> bool:
    """``func()`` 是否抛 ``ValueError``（用于分片入参校验）。"""
    try:
        func()
    except ValueError:
        return True
    return False


# ---------------------------------------------------------------------------
# 1) nDCG / recall / MRR 数值正确性
# ---------------------------------------------------------------------------


def _check_metrics(checker: _Checker) -> None:
    _section("[1/6] metric：nDCG@10 / recall / MRR 数值（手工可验算）")

    # 1. 单 gold，Top-1 命中 → 1.0
    checker.check_close("qrels={a:1}, ranked=[a] → nDCG=1.0", 1.0, metric.ndcg_at_k(["a"], {"a": 1.0}))

    # 2. 单 gold 排在第 2 → 1 / log2(3)
    checker.check_close(
        "qrels={a:1}, ranked=[b,a] → nDCG=1/log2(3)",
        1.0 / math.log2(3.0),
        metric.ndcg_at_k(["b", "a"], {"a": 1.0}),
    )

    # 3. 多档相关性：2^rel-1 加权（ranked=[a] 时 DCG=7，IDCG=7+1/log2(3)）
    expected = 7.0 / (7.0 + 1.0 / math.log2(3.0))
    checker.check_close(
        "qrels={a:3,b:1}, ranked=[a] → nDCG=7/(7+1/log2 3)",
        expected,
        metric.ndcg_at_k(["a"], {"a": 3.0, "b": 1.0}),
    )

    # 4. 无正例 → None（不允许返回 0.0 拉低均值）
    checker.check("qrels={} → nDCG=None", metric.ndcg_at_k(["a"], {}) is None)
    checker.check("qrels={a:0} → nDCG=None（score=0 不算正例）", metric.ndcg_at_k(["a"], {"a": 0.0}) is None)

    # 5. 正例排在 k 之外 → 0.0（是 0 分，不是 None）
    ranked = [f"x{i}" for i in range(10)] + ["a"]
    checker.check_close("gold 排在第 11 位 → nDCG@10 = 0.0", 0.0, metric.ndcg_at_k(ranked, {"a": 1.0}, k=10))

    # 6. recall@10：2 个正例命中 1 个 → 0.5
    checker.check_close("qrels={a,b}, ranked=[b] → recall@10=0.5", 0.5, metric.recall_at_k(["b"], {"a": 1.0, "b": 1.0}))

    # 7. MRR@10：首个正例在第 3 位 → 1/3
    checker.check_close("qrels={a}, ranked=[x,y,a] → MRR@10=1/3", 1.0 / 3.0, metric.mrr_at_k(["x", "y", "a"], {"a": 1.0}))

    # 8. precision@2：Top-2 命中 1 个 → 0.5
    checker.check_close("qrels={a}, ranked=[a,b] → precision@2=0.5", 0.5, metric.precision_at_k(["a", "b"], {"a": 1.0}, k=2))

    # 附加：summarize 跳过 None
    summary = metric.summarize([1.0, None, 0.0])
    checker.check(
        "metric.summarize 跳过 None",
        summary["n"] == 2 and summary["skipped"] == 1 and abs(summary["mean"] - 0.5) < 1e-12,
        json.dumps(summary),
    )


# ---------------------------------------------------------------------------
# 2) parse_json_tolerant
# ---------------------------------------------------------------------------


def _check_parse_tolerant(checker: _Checker) -> None:
    _section("[2/6] adapter.parse_json_tolerant（脏日志 / NDJSON / 内嵌数组）")

    clean = '[{"file_path": "docs/a.py"}]'
    checker.check("纯 JSON 数组", adapter.parse_json_tolerant(clean) == json.loads(clean))

    polluted = (
        "2026-11-12T10:00:00Z  INFO codeseek: loading index\n"
        "warning: cache miss\n"
        '[{"file_path": "docs/a.py", "score": 0.5}]\n'
        "done in 1.2s\n"
    )
    parsed = adapter.parse_json_tolerant(polluted)
    checker.check(
        "日志污染后仍能截取 [..] 解析",
        isinstance(parsed, list) and parsed and parsed[0]["file_path"] == "docs/a.py",
    )

    ndjson = '{"file_path": "docs/a.py"}\n{"file_path": "docs/b.py"}'
    parsed_ndjson = adapter.parse_json_tolerant(ndjson)
    checker.check(
        "NDJSON：返回第一个可解析对象",
        isinstance(parsed_ndjson, dict) and parsed_ndjson.get("file_path") == "docs/a.py",
    )

    embedded = 'prefix text [{"file_path": "docs/c.py"}] suffix text'
    parsed_embedded = adapter.parse_json_tolerant(embedded)
    checker.check(
        "内嵌在自然语言里的 [..]",
        isinstance(parsed_embedded, list) and parsed_embedded[0]["file_path"] == "docs/c.py",
    )

    checker.check("完全无 JSON → None", adapter.parse_json_tolerant("boom: not json") is None)
    checker.check("空串 → None", adapter.parse_json_tolerant("") is None)


# ---------------------------------------------------------------------------
# 3) resolve_doc_ids
# ---------------------------------------------------------------------------


def _check_resolve(checker: _Checker) -> None:
    _section("[3/6] adapter.resolve_doc_ids（绝对/相对/basename/符号名兜底 + 去重）")

    with tempfile.TemporaryDirectory(prefix="mtebcode-selftest-") as tmp:
        corpus = Path(tmp) / "corpora" / "alpha"
        (corpus / "docs" / "sub").mkdir(parents=True)
        for name in ("x.py", "y.py", "w.py", "sub/z.py"):
            (corpus / "docs" / name).write_text("pass\n", encoding="utf-8")

        docmap = {
            "docs/x.py": "d0",
            "docs/y.py": "d1",
            "docs/sub/z.py": "d2",
            "docs/w.py": "d3",
        }

        # 1) codeseek 常返回相对 cwd 的路径
        resolution = adapter.resolve_doc_ids([{"file_path": "docs/y.py"}], docmap, corpus)
        checker.check("相对路径直接命中 docmap", resolution.ranked == ["d1"], str(resolution.ranked))

        # 2) ./ 前缀
        resolution = adapter.resolve_doc_ids([{"file_path": "./docs/x.py"}], docmap, corpus)
        checker.check("./ 前缀不影响匹配", resolution.ranked == ["d0"], str(resolution.ranked))

        # 3) 绝对路径（含 corpus 前缀）
        absolute = f"{corpus}/docs/sub/z.py"
        resolution = adapter.resolve_doc_ids([{"path": absolute}], docmap, corpus)
        checker.check("绝对路径按 corpus 前缀剥离", resolution.ranked == ["d2"], str(resolution.ranked))

        # 4) 只给 basename → 反向匹配
        resolution = adapter.resolve_doc_ids([{"file_path": "sub/z.py"}], docmap, corpus)
        checker.check("子目录相对路径命中", resolution.ranked == ["d2"], str(resolution.ranked))
        resolution = adapter.resolve_doc_ids([{"file_path": "/elsewhere/very/deep/z.py"}], docmap, corpus)
        checker.check("陌生前缀 + basename 兜底", resolution.ranked == ["d2"], str(resolution.ranked))

        # 5) symbol 名恰好等于 doc_id
        resolution = adapter.resolve_doc_ids([{"name": "d3"}], docmap, corpus)
        checker.check("name 等于 doc_id 的兜底", resolution.ranked == ["d3"], str(resolution.ranked))

        # 6) 未映射
        resolution = adapter.resolve_doc_ids([{"file_path": "/tmp/unknown/qqq.py"}], docmap, corpus)
        checker.check(
            "无法映射时记 unmapped（不猜测）",
            resolution.ranked == [] and resolution.n_unmapped == 1,
            f"ranked={resolution.ranked} unmapped={resolution.n_unmapped}",
        )

        # 7) 去重保序（同一文件多个 chunk）
        duplicate = [
            {"file_path": "docs/x.py", "score": 0.9},
            {"file_path": "docs/x.py", "score": 0.8},
            {"file_path": "docs/y.py", "score": 0.7},
        ]
        resolution = adapter.resolve_doc_ids(duplicate, docmap, corpus)
        checker.check(
            "同一 doc 的多个 chunk 去重保序",
            resolution.ranked == ["d0", "d1"] and resolution.n_returned == 3 and resolution.n_mapped == 3,
            f"ranked={resolution.ranked} returned={resolution.n_returned}",
        )


# ---------------------------------------------------------------------------
# 4) 数据层纯函数
# ---------------------------------------------------------------------------


def _check_data_helpers(checker: _Checker) -> None:
    _section("[4/6] data：文件名安全化 / 物化 / docmap / tasks 往返 / layout 探测")

    name = data.safe_doc_filename("d1", ".py")
    checker.check(
        "safe_doc_filename 只含安全字符且带短哈希",
        all(char.isalnum() or char in "._-" for char in name) and name.endswith(".py") and len(name) > 8,
        name,
    )
    weird = data.safe_doc_filename("a/b c:d?e", ".txt")
    checker.check("非法字符被替换", "/" not in weird and " " not in weird and "?" not in weird, weird)
    checker.check(
        "同一 doc_id 命名稳定",
        data.safe_doc_filename("d1", ".py") == data.safe_doc_filename("d1", ".py"),
    )

    spec = data.TASKS["CosQA"]
    with tempfile.TemporaryDirectory(prefix="mtebcode-selftest-") as tmp:
        destination = Path(tmp) / "cosqa"
        rows = [
            {"_id": "d1", "text": "def foo():\n    return 1\n", "title": "helper"},
            {"_id": "d2", "text": "def bar():\n    return 2\n", "title": ""},
        ]
        docmap, written = data.materialize_docs(spec, rows, destination, force=True)
        checker.check("materialize_docs 写出 2 个文件", written == 2 and len(docmap) == 2, f"written={written}")
        first = destination / next(iter(docmap))
        body = first.read_text(encoding="utf-8")
        checker.check(
            "代码文件首行加注释（.py 用 #）",
            body.startswith("# helper") and "def foo" in body,
            repr(body.splitlines()[0][:24]),
        )
        checker.check(
            "docmap.json 可读回且与返回值一致",
            json.loads((destination / data.DOCMAP_NAME).read_text(encoding="utf-8")) == docmap,
        )

        # 官方 MTEB 数据集的主键叫 `id`（没有 `_id`）：物化必须照样工作，否则 CSN 语料为空
        id_dir = Path(tmp) / "mteb-ids"
        id_map, id_written = data.materialize_docs(
            spec,
            [{"id": "mteb-doc-1", "text": "def baz():\n    return 3\n", "title": ""}],
            id_dir,
            force=True,
        )
        checker.check(
            "corpus 主键兼容 `id`（mteb/CodeSearchNetRetrieval 只有 id）",
            id_written == 1 and list(id_map.values()) == ["mteb-doc-1"],
            json.dumps(id_map, ensure_ascii=False),
        )

        sql_spec = data.TASKS["SyntheticText2SQL"]
        sql_dir = Path(tmp) / "t2s"
        data.materialize_docs(sql_spec, [{"_id": "s1", "text": "SELECT 1", "title": "ignored"}], sql_dir, force=True)
        sql_body = (sql_dir / next(iter(json.loads((sql_dir / data.DOCMAP_NAME).read_text(encoding="utf-8"))))).read_text(
            encoding="utf-8"
        )
        checker.check(".sql 不插入注释（不污染 SQL）", sql_body == "SELECT 1", repr(sql_body[:24]))

        task = data.Task(
            task_id="cosqa__q0000",
            task="CosQA",
            hf_dataset="CoIR-Retrieval/cosqa",
            query_id="q1",
            query_text="how to sort a dict",
            gold_doc_ids=["d1"],
            corpus_dir=str(destination),
            docmap_path=str(destination / data.DOCMAP_NAME),
            n_pool_docs=len(docmap),
            qrels={"d1": 1.0},
            qrels_split="test",
            language="python",
        )
        tasks_path = data.write_tasks([task], Path(tmp) / data.TASKS_FILE)
        loaded = data.load_tasks(tasks_path)
        checker.check(
            "tasks.jsonl 往返（qrels/split 都保留）",
            len(loaded) == 1 and loaded[0].qrels == {"d1": 1.0} and loaded[0].qrels_split == "test",
        )

    layout = data.detect_layout(
        [
            {"config": "corpus", "split": "corpus"},
            {"config": "queries", "split": "queries"},
            {"config": "default", "split": "train"},
            {"config": "default", "split": "test"},
        ]
    )
    checker.check(
        "detect_layout 推断 corpus/queries/default",
        layout is not None and layout.qrels_config == "default" and layout.qrels_splits == ("test", "train"),
        str(layout),
    )

    checker.check(
        "默认集 = 仅 CosQA（不带 --tasks 也能直接跑 prepare）",
        data.DEFAULT_TASKS == ("CosQA",),
        ",".join(data.DEFAULT_TASKS),
    )
    checker.check(
        "DEFAULT_TASKS 不含任何带 excluded_reason 的任务",
        not [name for name in data.DEFAULT_TASKS if data.TASKS[name].excluded_reason],
        ",".join(data.DEFAULT_TASKS),
    )
    checker.check(
        "注册表 = 9 个既有任务 + 100 个 CSN 分片，且都有 doc_ext",
        len(data.TASKS) == 109 and all(spec.doc_ext.startswith(".") for spec in data.TASKS.values()),
        f"{len(data.TASKS)} tasks",
    )

    # ---- 第 2 批：diff → 后像代码规整（CodeEditSearchRetrieval 的 corpus 全是 unified diff）----
    raw_diff = (
        "--- \n"
        "+++ \n"
        "@@ -3,3 +3,5 @@\n"
        " \n"
        " from constructors import array\n"
        "-old_helper = 1\n"
        "+def test(verbosity=1):\n"
        "+    return 1\n"
        "\\ No newline at end of file\n"
    )
    checker.check(
        "diff_postimage：丢弃 @@ / - / --- / +++ / \\ 标记，保留 + 与上下文行",
        data.diff_postimage(raw_diff)
        == "\nfrom constructors import array\ndef test(verbosity=1):\n    return 1\n",
        repr(data.diff_postimage(raw_diff)),
    )
    indent_diff = "@@ -1,2 +1,2 @@\n def f():\n-    pass\n+    return 1\n"
    checker.check(
        "diff_postimage 保留相对缩进（只剥 1 个补丁前缀字符）",
        data.diff_postimage(indent_diff) == "def f():\n    return 1\n",
        repr(data.diff_postimage(indent_diff)),
    )

    # ---- 100 个 CodeSearchNetRetrieval 语言分片（取代 CodeEditSearchRetrieval 系列）----
    csn_names = list(data.CSN_TASK_NAMES)
    checker.check(
        "CSN 分片共 100 个且全部注册在 TASKS 里",
        len(csn_names) == 100 and all(name in data.TASKS for name in csn_names),
        f"{len(csn_names)} 个分片 / {sum(1 for name in csn_names if name in data.TASKS)} 已注册",
    )
    lang_counts: dict[str, int] = {}
    for name in csn_names:
        lang_counts[data.TASKS[name].language] = lang_counts.get(data.TASKS[name].language, 0) + 1
    checker.check(
        "语言分布恰为 go 34 / java 33 / python 33",
        lang_counts == {"go": 34, "java": 33, "python": 33},
        json.dumps(lang_counts, sort_keys=True),
    )
    csn_runnable = [
        data.TASKS[name]
        for name in csn_names
        if not data.TASKS[name].excluded_reason and not data.TASKS[name].unverified
    ]
    checker.check(
        "100 个分片全部可跑（无 excluded_reason/unverified），且 full_corpus=True、query_quota=0",
        len(csn_runnable) == 100
        and all(spec.full_corpus and spec.query_quota == 0 for spec in csn_runnable),
        f"可跑 {len(csn_runnable)} 个",
    )
    csn_dirs = {data.corpus_dir("/tmp/x", data.TASKS[name]) for name in csn_names}
    checker.check(
        "100 个分片的 corpus_dir() 去重后恰好 3 个（共享语料 = 3 次 codeseek init）",
        len(csn_dirs) == 3,
        ", ".join(sorted(path.name for path in csn_dirs)),
    )
    csn_layout = data.TASKS[csn_names[0]].layout
    checker.check(
        "数据源 = mteb/CodeSearchNetRetrieval，三个 config 的 split 均为 test",
        data.TASKS[csn_names[0]].hf_dataset == "mteb/CodeSearchNetRetrieval"
        and csn_layout is not None
        and csn_layout.corpus_split == "test"
        and csn_layout.queries_split == "test"
        and csn_layout.qrels_splits == ("test",),
        f"{data.TASKS[csn_names[0]].hf_dataset} / {csn_layout}",
    )
    forbidden_langs = sorted({data.TASKS[name].language for name in csn_names} - {"go", "java", "python"})
    forbidden_names = [
        name for name in csn_names if any(token in name for token in ("javascript", "php", "ruby"))
    ]
    checker.check(
        "不存在 javascript / php / ruby 分片",
        not forbidden_langs and not forbidden_names,
        ", ".join(forbidden_langs + forbidden_names) or "无",
    )
    codeedit_leftover = [name for name in data.TASKS if name.startswith("CodeEditSearchRetrieval")]
    checker.check(
        "注册表里不再存在任何 CodeEditSearchRetrieval 前缀的任务",
        not codeedit_leftover and "CodeSearchNetRetrieval" not in data.TASKS,
        ", ".join(codeedit_leftover) or "无",
    )

    # ---- shard_query_ids：确定性互斥切分（纯函数，1000 条 query 覆盖全部 3 个语言）----
    ids_1000 = [f"q{index:04d}" for index in range(1000)]
    for total in (34, 33):
        shards = [data.shard_query_ids(ids_1000, index, total) for index in range(total)]
        lengths = [len(shard) for shard in shards]
        union = sorted(item for shard in shards for item in shard)
        checker.check(
            f"shard_query_ids：{total} 片长度差 <= 1、两两互斥、并集 = 全集",
            max(lengths) - min(lengths) <= 1 and sum(lengths) == 1000 and union == sorted(ids_1000),
            f"lengths {min(lengths)}..{max(lengths)}, union {len(set(union))}",
        )
    checker.check(
        "shard_query_ids：越界 / total<=0 抛 ValueError，total=1 返回全集",
        _raises_value_error(lambda: data.shard_query_ids(ids_1000, 34, 34))
        and _raises_value_error(lambda: data.shard_query_ids(ids_1000, -1, 33))
        and _raises_value_error(lambda: data.shard_query_ids(ids_1000, 0, 0))
        and data.shard_query_ids(ids_1000, 0, 1) == sorted(ids_1000),
    )

    # diff_postimage() 保留为工具 + 单测（上面两条），但**没有任何任务再用它**：
    # CSN 的 corpus 是真实源码。这里用一个临时 spec 覆盖「物化路径确实会调用它」。
    diff_spec = data.TaskSpec(
        name="SelftestDiffPostimage",
        hf_dataset="selftest/diff",
        layout=None,
        doc_ext=".py",
        code_like=True,
        language="python",
        diff_postimage=True,
    )
    checker.check(
        "不再有注册任务使用 diff_postimage（CSN corpus 是真实源码）",
        not [name for name in data.TASKS if data.TASKS[name].diff_postimage],
    )
    prepared_rows = [
        {"id": "h1", "text": "@@ -1 +1 @@\n-x = 1\n+x = 2\n", "title": ""},
    ]
    with tempfile.TemporaryDirectory(prefix="mtebcode-selftest-") as tmp:
        data.materialize_docs(diff_spec, prepared_rows, Path(tmp) / "ce", force=True)
        written = (
            Path(tmp) / "ce" / next(iter(json.loads((Path(tmp) / "ce" / data.DOCMAP_NAME).read_text(encoding="utf-8"))))
        ).read_text(encoding="utf-8")
        checker.check("物化时按 diff_postimage 落盘（不含 @@ 与 - 行）", written == "x = 2\n", repr(written))

    excluded_without_reason = [
        name
        for name in (
            "AppsRetrieval",
            "CodeTransOceanContest",
            "CodeTransOceanDL",
            "CodeFeedbackST",
            "CodeFeedbackMT",
            "StackOverflowQA",
            "SyntheticText2SQL",
        )
        if not data.TASKS[name].excluded_reason
    ]
    checker.check(
        "7 个已剔除任务都带 excluded_reason",
        not excluded_without_reason,
        ",".join(excluded_without_reason) or "无",
    )
    checker.check(
        "采样指纹稳定（同输入同输出）",
        data.sampling_fingerprint(
            [
                data.Task(
                    task_id="t__q0000",
                    task="T",
                    hf_dataset="d",
                    query_id="q",
                    query_text="x",
                    gold_doc_ids=["a", "b"],
                    corpus_dir="/tmp",
                    docmap_path="/tmp/docmap.json",
                    n_pool_docs=2,
                )
            ],
            42,
            300,
            30,
        )
        == data.sampling_fingerprint(
            [
                data.Task(
                    task_id="t__q0000",
                    task="T",
                    hf_dataset="d",
                    query_id="q",
                    query_text="x",
                    gold_doc_ids=["b", "a"],
                    corpus_dir="/tmp",
                    docmap_path="/tmp/docmap.json",
                    n_pool_docs=2,
                )
            ],
            42,
            300,
            30,
        ),
    )


# ---------------------------------------------------------------------------
# 5) 端到端：fake raw → score_tasks → build_report → write_reports
# ---------------------------------------------------------------------------


def _fake_workdir(root: Path) -> tuple[list[data.Task], Path]:
    """构造两个任务 / 5 条 query 的假工作目录（含真实语料文件与 docmap）。"""
    corpora = root / "corpora"
    docs = {"d0": "x", "d1": "y", "d2": "z", "d3": "w"}
    corpus_dir = corpora / "alpha"
    (corpus_dir / "docs").mkdir(parents=True)
    docmap: dict[str, str] = {}
    for doc_id, stem in docs.items():
        relative = f"docs/{stem}.py"
        (corpus_dir / relative).write_text(f"# {doc_id}\ndef f_{doc_id}():\n    pass\n", encoding="utf-8")
        docmap[relative] = doc_id
    (corpus_dir / data.DOCMAP_NAME).write_text(json.dumps(docmap), encoding="utf-8")
    # codeseek init 的硬前置
    (corpus_dir / ".git").mkdir()

    beta_dir = corpora / "beta"
    (beta_dir / "docs").mkdir(parents=True)
    beta_map = {"docs/x.py": "d0", "docs/y.py": "d1", "docs/z.py": "d2", "docs/w.py": "d3"}
    for relative in beta_map:
        (beta_dir / relative).write_text("# beta\n", encoding="utf-8")
    (beta_dir / data.DOCMAP_NAME).write_text(json.dumps(beta_map), encoding="utf-8")

    def make(task: str, index: int, qid: str, corpus: Path, map_: dict[str, str], qrels: dict[str, float]) -> data.Task:
        return data.Task(
            task_id=f"{task.lower()}__q{index:04d}",
            task=task,
            hf_dataset="fake/dataset",
            query_id=qid,
            query_text=f"query {qid}",
            gold_doc_ids=sorted(qrels),
            corpus_dir=str(corpus),
            docmap_path=str(corpus / data.DOCMAP_NAME),
            n_pool_docs=len(map_),
            qrels=qrels,
            qrels_split="test",
            language="python",
        )

    tasks = [
        make("Alpha", 0, "q1", corpus_dir, docmap, {"d0": 1.0}),
        make("Alpha", 1, "q2", corpus_dir, docmap, {"d0": 1.0}),
        make("Alpha", 2, "q3", corpus_dir, docmap, {}),
        make("Beta", 0, "q1", beta_dir, beta_map, {"d0": 1.0, "d1": 1.0}),
        make("Beta", 1, "q2", beta_dir, beta_map, {"d0": 1.0}),
    ]

    raws: dict[str, dict[str, Any]] = {
        # Alpha/q1：Top-1 命中 → nDCG 1.0
        "alpha__q0000": {
            "mode": adapter.MODE_HYBRID,
            "results": [{"file_path": "docs/x.py", "score": 0.9}],
            "results_count": 1,
            "error": "",
            "elapsed_s": 0.01,
            "retrieved_at": "selftest",
        },
        # Alpha/q2：gold 排第 2 → 1/log2(3)
        "alpha__q0001": {
            "mode": adapter.MODE_HYBRID,
            "results": [{"file_path": "docs/y.py"}, {"file_path": "docs/x.py"}],
            "results_count": 2,
            "error": "",
            "elapsed_s": 0.02,
            "retrieved_at": "selftest",
        },
        # Alpha/q3：无正例 → 指标 None，被跳过
        "alpha__q0002": {
            "mode": adapter.MODE_FALLBACK,
            "results": [{"file_path": "docs/y.py"}],
            "results_count": 1,
            "error": "",
            "elapsed_s": 0.01,
            "retrieved_at": "selftest",
        },
        # Beta/q1：两个正例都在 Top-2 → nDCG 1.0
        "beta__q0000": {
            "mode": adapter.MODE_HYBRID,
            "results": [{"file_path": "docs/x.py"}, {"file_path": "docs/y.py"}],
            "results_count": 2,
            "error": "",
            "elapsed_s": 0.01,
            "retrieved_at": "selftest",
        },
        # Beta/q2：完全没命中 → 0.0
        "beta__q0001": {
            "mode": adapter.MODE_HYBRID,
            "results": [{"file_path": "docs/z.py"}, {"file_path": "docs/w.py"}],
            "results_count": 2,
            "error": "",
            "elapsed_s": 0.01,
            "retrieved_at": "selftest",
        },
    }
    return tasks, root, raws  # type: ignore[return-value]


def _check_end_to_end(checker: _Checker) -> None:
    _section("[5/6] 端到端：score_tasks → runner.score_only/run 聚合 → report.json/report.md")

    with tempfile.TemporaryDirectory(prefix="mtebcode-selftest-") as tmp:
        root = Path(tmp) / "workdir"
        tasks, root, raws = _fake_workdir(root)
        raw_dir = root / data.RAW_DIR
        raw_dir.mkdir(parents=True, exist_ok=True)
        for task_id, payload in raws.items():
            (raw_dir / f"{task_id}.json").write_text(json.dumps(payload), encoding="utf-8")
        data.write_tasks(tasks, root / data.TASKS_FILE)

        scoring = scorer.score_tasks(tasks, raws, k=10, search_limit=20)
        by_task = scoring["by_task"]
        expected_alpha = (1.0 + 1.0 / math.log2(3.0)) / 2.0
        checker.check_close("Alpha 任务均值 = (1 + 1/log2 3)/2", expected_alpha, by_task["Alpha"]["ndcg_at_10"])
        checker.check_close("Beta 任务均值 = (1 + 0)/2", 0.5, by_task["Beta"]["ndcg_at_10"])
        expected_overall = (expected_alpha + 0.5) / 2.0
        checker.check_close(
            "总体 = 任务级均值的算术平均（Mean(Task)）",
            expected_overall,
            scoring["aggregate"]["mean_ndcg_at_10"],
        )
        checker.check(
            "无正例 query 被跳过而非记 0",
            scoring["aggregate"]["n_queries_scored"] == 4 and scoring["aggregate"]["n_queries_skipped"] == 1,
            f"scored={scoring['aggregate']['n_queries_scored']} skipped={scoring['aggregate']['n_queries_skipped']}",
        )
        language_bucket = scoring["by_language"].get("python") or {}
        checker.check(
            "by_language：单一语言聚合 2 个任务，mean_of_task_means = 总体 Mean(Task)",
            set(scoring["by_language"]) == {"python"}
            and language_bucket.get("n_tasks") == 2
            and language_bucket.get("mean_of_task_means") is not None
            and abs(float(language_bucket["mean_of_task_means"]) - expected_overall) < 1e-12,
            json.dumps(
                {
                    language: {
                        "n_tasks": bucket.get("n_tasks"),
                        "mean_of_task_means": bucket.get("mean_of_task_means"),
                    }
                    for language, bucket in scoring["by_language"].items()
                },
                sort_keys=True,
            ),
        )

        # score 子命令路径
        summary = runner.score_only(tasks=tasks, workdir=root, k=10, search_limit=20)
        checker.check_close(
            "runner.score_only 与直接评分一致",
            expected_overall,
            summary.report["aggregate"]["mean_ndcg_at_10"],
        )
        checker.check(
            "name_fallback 触发醒目的检索模式警示",
            any("name_fallback" in warning for warning in summary.report["warnings"]),
            str(len(summary.report["warnings"])),
        )
        checker.check(
            "Top-10 worst queries 非空且 ≤ 10",
            0 < len(summary.report["failure_top"]) <= 10,
            f"{len(summary.report['failure_top'])} 条",
        )
        worst_task = summary.report["failure_top"][0]
        checker.check(
            "worst query 是 nDCG=0.0 的 Beta/q2",
            worst_task["task_id"] == "beta__q0001",
            str(worst_task["task_id"]),
        )

        json_path = Path(summary.report_json_path)
        md_path = Path(summary.report_md_path)
        report_json = json.loads(json_path.read_text(encoding="utf-8"))
        checker.check("report.json 可被 json.loads 且为 dict", isinstance(report_json, dict))

        required_top = {"meta", "aggregate", "per_query", "failure_top", "warnings"}
        checker.check("report.json 顶层字段齐全", required_top <= set(report_json), str(sorted(report_json)))
        required_meta = {
            "generated_at",
            "benchmark",
            "tasks",
            "n_queries",
            "n_docs_total",
            "k",
            "search_limit",
            "dataset_splits",
            "mode_counts",
            "command",
            "seed",
            "embedding",
        }
        meta = report_json["meta"]
        checker.check("report.meta 字段齐全", required_meta <= set(meta), str(sorted(required_meta - set(meta))))
        checker.check("report.meta.benchmark = MTEB(Code, v1)", meta.get("benchmark") == data.BENCHMARK, str(meta.get("benchmark")))
        checker.check(
            "report.meta.mean nDCG 与 aggregate 一致",
            abs(float(report_json["aggregate"]["mean_ndcg_at_10"]) - expected_overall) < 1e-12,
        )
        checker.check(
            "aggregate.by_task 含两个任务",
            set(report_json["aggregate"]["by_task"]) == {"Alpha", "Beta"},
            str(sorted(report_json["aggregate"]["by_task"])),
        )
        checker.check(
            "report.json aggregate.by_language 带出语言层聚合",
            set(report_json["aggregate"].get("by_language") or {}) == {"python"},
            str(sorted(report_json["aggregate"].get("by_language") or {})),
        )

        markdown = md_path.read_text(encoding="utf-8")
        headings = (
            "## 1. Summary",
            "## 2. Retrieval Mode Statistics",
            "## 3. Per-Task Breakdown",
            "## 4. Top 10 Worst Queries",
            "## 5. Reproduction Commands",
        )
        checker.check(
            "report.md 含 5 个规定小节",
            all(heading in markdown for heading in headings),
            ", ".join(heading for heading in headings if heading not in markdown) or "全部命中",
        )
        checker.check(
            "report.md 顶部有 name_fallback blockquote 警告",
            "> ⚠️" in markdown and "name_fallback" in markdown.split("## 1. Summary")[0],
        )
        checker.check(
            "report.md 保留 4 位小数",
            f"{expected_overall:.4f}" in markdown,
            f"{expected_overall:.4f}",
        )
        checker.check(
            "report.md 含按语言汇总表（### By Language）",
            "### By Language" in markdown and "| Language | Shards |" in markdown,
        )
        alpha_at = markdown.find("| Alpha |")
        beta_at = markdown.find("| Beta |")
        checker.check(
            "report.md 的 per-task 表格按任务名排序（Alpha 在 Beta 之前）",
            0 <= alpha_at < beta_at,
            f"alpha@{alpha_at} beta@{beta_at}",
        )
        print("\n  --- report.md 前 12 行（节选）---")
        for line in markdown.splitlines()[:12]:
            print(f"  | {line}")


# ---------------------------------------------------------------------------
# 6) CLI 装配 + 容错
# ---------------------------------------------------------------------------


def _check_cli_and_robustness(checker: _Checker) -> None:
    _section("[6/6] CLI 装配 / 错误路径 / --limit-queries 语义")

    from . import cli

    parser = cli.build_parser()
    for command in ("prepare", "run", "score", "selftest"):
        try:
            namespace = parser.parse_args([command])
            ok = callable(getattr(namespace, "func", None))
        except SystemExit:
            ok = False
        checker.check(f"子命令 {command} 可解析", ok)

    tasks = [
        data.Task(
            task_id=f"alpha__q{i:04d}",
            task="Alpha",
            hf_dataset="fake",
            query_id=f"q{i}",
            query_text="x",
            gold_doc_ids=[],
            corpus_dir="/tmp",
            docmap_path="/tmp/docmap.json",
            n_pool_docs=1,
        )
        for i in range(4)
    ] + [
        data.Task(
            task_id=f"beta__q{i:04d}",
            task="Beta",
            hf_dataset="fake",
            query_id=f"q{i}",
            query_text="x",
            gold_doc_ids=[],
            corpus_dir="/tmp",
            docmap_path="/tmp/docmap.json",
            n_pool_docs=1,
        )
        for i in range(4)
    ]
    limited = cli._limit_tasks_per_task(tasks, 2)
    checker.check(
        "--limit-queries 按任务截断（每任务 2 条 → 共 4 条）",
        len(limited) == 4 and {task.task for task in limited} == {"Alpha", "Beta"},
        f"{len(limited)} 条",
    )

    # 未知任务名必须报错而不是崩溃
    try:
        cli._parse_tasks("CosQA,NoSuchTask")
        unknown_rejected = False
    except Exception as exc:  # noqa: BLE001 - 只为验证「明确报错」
        unknown_rejected = "未知任务" in str(exc)
    checker.check("未知任务名被明确拒绝", unknown_rejected)

    # --tasks csn 别名
    csn_expanded = cli._parse_tasks("csn")
    checker.check(
        "--tasks csn 展开为全部 100 个 CSN 分片（与 CSN_TASK_NAMES 一致）",
        len(csn_expanded) == 100 and set(csn_expanded) == set(data.CSN_TASK_NAMES),
        f"{len(csn_expanded)} 个",
    )
    checker.check(
        "--tasks csn,CosQA 别名与显式任务名可混用且去重保序",
        cli._parse_tasks("csn,CosQA") == list(data.CSN_TASK_NAMES) + ["CosQA"]
        and len(cli._parse_tasks("CosQA,csn,CosQA")) == 101,
    )

    # 缺少 docmap 时 scorer 不崩，只记 error
    broken = data.Task(
        task_id="broken__q0000",
        task="Broken",
        hf_dataset="fake",
        query_id="q0",
        query_text="x",
        gold_doc_ids=["d0"],
        corpus_dir="/nonexistent",
        docmap_path="/nonexistent/docmap.json",
        n_pool_docs=1,
        qrels={"d0": 1.0},
    )
    scoring = scorer.score_tasks([broken], {"broken__q0000": {"results": [{"file_path": "docs/x.py"}]}})
    row = scoring["per_query"][0]
    checker.check(
        "docmap 缺失 → 记 error 且指标为 0.0（不崩）",
        bool(row["error"]) and row["ndcg_at_10"] == 0.0,
        f"error={row['error'][:40]!r} ndcg={row['ndcg_at_10']}",
    )

    # missing raw → FileNotFoundError（CLI 会转成退出码 2）
    with tempfile.TemporaryDirectory(prefix="mtebcode-selftest-") as tmp:
        try:
            runner.score_only([broken], tmp)
            raised = False
        except FileNotFoundError:
            raised = True
        checker.check("缺少 raw 结果时 score_only 明确报错", raised)


# ---------------------------------------------------------------------------
# 入口
# ---------------------------------------------------------------------------


def run_selftest() -> int:
    """跑全部离线自检；返回 0 表示全部通过，1 表示有失败。"""
    print("=== MTEB(Code, v1) × codeseek eval selftest（离线：不需要网络 / codeseek / numpy）===")
    print(f"mtebcode_eval version = {__import__('mtebcode_eval').__version__}")
    checker = _Checker()

    _check_metrics(checker)
    _check_parse_tolerant(checker)
    _check_resolve(checker)
    _check_data_helpers(checker)
    _check_end_to_end(checker)
    _check_cli_and_robustness(checker)

    print("\n=== 汇总 ===")
    print(f"通过 {checker.passes} 项，失败 {len(checker.failures)} 项")
    if checker.failures:
        for name, detail in checker.failures:
            print(f"  - FAILED: {name}" + (f"  |  {detail}" if detail else ""))
        return 1
    print("SELFTEST OK")
    return 0
