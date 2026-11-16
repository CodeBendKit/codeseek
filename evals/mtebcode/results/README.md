# MTEB(Code, v1) × codeseek measured results archive

> **Primary result: the `2026-11-13` report, mean nDCG@10 = 0.5858** (100 queries / 4 tasks), binary under test
> `sha256` prefix `b1532bb1b43e3d65`, evaluation condition **`index.hybrid.cutoff.enabled = false`**.
> The body of this document (key takeaways / task and language selection / evaluation conditions / key insights /
> known limitations) **refers to that batch throughout**.

This archives codeseek's end-to-end retrieval results on a **100-query subset** of MTEB(Code, v1)-style tasks
(measured, not estimated and not extrapolated from a sample). It holds a single batch: the
**2026-11-13 short-query result (mean nDCG@10 = 0.5858)**.

> ℹ️ **The `CE-*` (`CodeEditSearchRetrieval-*`) task family has been replaced by 100
> `CodeSearchNetRetrieval-<lang>-sNN` shards** (go 34 / java 33 / python 33, `mteb/CodeSearchNetRetrieval`,
> shared corpus per language). Those tasks are no longer registered, so this batch is **history only** and its
> numbers are not comparable with a CSN batch — the corpus, the queries and the task set all changed.
> See `../README.md` for the current protocol.

| file | description |
| --- | --- |
| `2026-11-13-report.md` | **Primary result**: human-readable report (summary / retrieval-mode statistics / per-task breakdown / top-10 worst queries / reproduction commands) |
| `2026-11-13-report.json` | **Primary result**: machine-readable detail (`meta` / `aggregate` / `per_query` / `failure_top` / `warnings`) |
| `README.md` | This file: archive index, task and language selection, evaluation conditions, insights, limitations |

**Provenance**: `2026-11-13-report.md` is produced by `render_markdown()` in `mtebcode_eval/report.py` from
`2026-11-13-report.json`; **no number was edited by hand**, and `md == render_markdown(json)` was verified
**byte for byte** (`True`). Measured values of the archived files:

| file | bytes | md5 |
| --- | --- | --- |
| `2026-11-13-report.md` | 8383 | `23da8ea3d96b006aa26eb1077ab52c86` |
| `2026-11-13-report.json` | 103687 | `37011b296da2498ea7a01b9cc52fda33` |

The `.json` file was **never rewritten** after the run: only the `.md` was rendered from that same `.json`.
Every statistic below is read from the `.json`.

**Batch**: 2026-11-13 — 100 queries, 4 tasks, 300 pooled documents per task, `index.hybrid.cutoff.enabled = false`.

`CE-*` = `CodeEditSearchRetrieval-{python,go,typescript}`. Per-task query counts: CosQA **40**, the three
CodeEditSearch tasks **20 each**. The overall number is the **arithmetic mean of the four per-task means**
(MTEB `Mean(Task)`), **not** a pooled mean over 100 queries:
`(0.969046… + 0.413093… + 0.489344… + 0.471534…) / 4 = 0.585754…` -> `0.5858`.

## 1. Key takeaways

**Mean nDCG@10 = 0.5858** across 4 tasks and 100 queries:

| task | queries | nDCG@10 | recall@10 | MRR@10 | recall@20 |
| --- | --- | --- | --- | --- | --- |
| CosQA | 40 | 0.9690 | 1.0000 | 0.9583 | 1.0000 |
| CodeEditSearchRetrieval-python | 20 | 0.4131 | 0.5000 | 0.3833 | 0.5000 |
| CodeEditSearchRetrieval-go | 20 | 0.4893 | 0.5500 | 0.4708 | 0.5500 |
| CodeEditSearchRetrieval-typescript | 20 | 0.4715 | 0.5000 | 0.4625 | 0.5000 |
| **Mean(Task)** | **100** | **0.5858** | **0.6375** | **0.5688** | **0.6375** |

All four metrics are computed at `k = 10` except the last column, which uses the actual search budget
(`search_limit = 20`). Four decimals are kept because nDCG differences are much finer than
percentage-point accuracy.

### 1.1 Derived: what the numbers look like *conditional on a hit*

| derived metric | computation | value |
| --- | --- | --- |
| conditional nDCG@10 (over recalled queries only) | `0.5858 / 0.6375` | **0.9188** |
| conditional MRR@10 (over recalled queries only) | `0.5688 / 0.6375` | **0.8922** |
| mean rank of the gold document, given it was recalled | `1 / 0.8922` | **1.12** |

**Once the gold document is recalled, ordering is near-optimal**: a mean rank of 1.12 means the gold document is
almost always the first result, and every remaining point is lost in *retrieval*, not in *ranking*.

> The division above is done on the 4-decimal values printed in the tables. Dividing the unrounded means
> (`0.5857543695 / 0.6375`) gives `0.9188`; `0.5687500 / 0.6375` gives `0.892157`, i.e. a mean rank of `1.1209`.

### 1.2 Retrieval mode statistics

| mode | queries | share | meaning |
| --- | --- | --- | --- |
| `hybrid` | **100** | 100.0% | true semantic retrieval (LanceDB + BM25, results carry a `code_block`) |
| `name_fallback` | 0 | 0.0% | degradation to function-name substring matching, used when `embedding.api_token` is absent |
| `unknown_empty` | 0 | 0.0% | empty or unparsable search results, impossible to judge |

`mean_returned = 20.00`, `mean_mapped = 20.00`, `n_unmapped = 0`: every query returned the full budget and
**every single result mapped back to a `doc_id`**, so no ranking position was lost to mapping failures.

### 1.3 Failure decomposition

| component | value | meaning |
| --- | --- | --- |
| `1 − recall@10` | **0.3625** | outright recall failure (the gold document never reaches Top-10) |
| `recall@10 − nDCG@10` | **0.0517** | ordering loss **after** the gold document was retrieved |
| `nDCG@10` | 0.5858 | the two components above, subtracted from 1.0 |

Of the **41.42 points** missing at the top of the scale, **36.25 are entirely recall** and only **5.17** are a
ranking problem (`41.42 = 36.25 + 5.17 = 1 − nDCG@10`).

## 2. Task and language selection

This section records **how the task set and the language set were filtered**, i.e. what makes the four archived
tasks measurable at all. Every filter below is a **dataset-level** property (query length, corpus shape, extension
whitelist, availability of qrels) screened before sampling; none of them is a score.

### 2.1 Why only these four tasks survived

Query length was the first filter. The numbers below were measured per task during screening and are recorded
in the task registry (`mtebcode_eval/data.py`, `TaskSpec.excluded_reason` / `note`).

| task | query length (dataset level) | verdict |
| --- | --- | --- |
| CosQA | short (natural-language questions such as `python check file is readonly`) | **adopted** (quota raised to 40 so the batch reaches 100 queries) |
| CodeEditSearchRetrieval-python | 21–1,811 chars, median **45** | **adopted** |
| CodeEditSearchRetrieval-go | 19–1,529 chars, median **45** | **adopted** |
| CodeEditSearchRetrieval-typescript | 19–659 chars, median **44**; only **3.6%** exceed 200 chars | **adopted** |
| SyntheticText2SQL | short | **rejected**: the corpus is `.sql`, outside codeseek's extension whitelist (`rust-core/src/codegraph/parser.rs` `is_supported_file`), so the index holds 0 files |
| CodeFeedbackST | median 540 | **rejected**: query too long |
| CodeTransOceanContest | median 547 | **rejected**: query too long |
| CodeSearchNetRetrieval | median 624 | **rejected**: query too long, **and** the corpus text is the docstring (only 12.7% code-like) -> `codeseek init` indexed **1 file / 3 symbols** |
| StackOverflowQA | median 883 | **rejected**: query too long (34.2% of the query text is prose) |
| CodeTransOceanDL | median 1,211 | **rejected**: query too long |
| AppsRetrieval | median 1,316 | **rejected**: query too long (98.9% of queries exceed 200 chars) |
| CodeFeedbackMT | median 3,571 | **rejected**: query too long |

> **Scope warning.** "Dataset level" and "as evaluated" are **not** the same population. The **evaluated sample**
> of 100 queries, as recorded in the archived `per_query` array, is much shorter: median **33.5** chars for CosQA,
> **44** for CodeEditSearch-go, **52** for -typescript, **51** for -python; across all 100 queries the median is
> **40** and the maximum is **233** (5.0% of the python queries and 5.0% of the typescript queries exceed 200
> chars, 0.0% of go/CosQA). The screening table above describes the datasets; the sample is drawn from their
> short-query body.

### 2.2 Language variants of CodeEditSearchRetrieval

| language | corpus / queries / qrels | verdict |
| --- | --- | --- |
| python | all three endpoints HTTP 200, 1,000 rows each | **adopted** |
| go | all three endpoints HTTP 200, 1,000 rows each | **adopted** |
| typescript | `datasets-server` `/rows` returns **500** for all three configs, but the dataset's **official parquet** files are complete and readable (corpus 320,945 B / queries 48,594 B / qrels 48,818 B, 1,000 rows each); `.ts` and `.tsx` are both inside codeseek's whitelist | **adopted** — read through `--parquet-cache`, never through `/rows` |
| java | `java-corpus` and `java-queries` exist, **`java-qrels` does not** (`/rows` returns 404) | **rejected**: `java-qrels config does not exist in the dataset (404)` — no relevance judgements, and codeseek does index `.java`; the missing half is the qrels |
| ruby | all three configs return **500** (`/rows` cannot read the parquet); `.rb` is also outside the whitelist | **rejected**: `all three configs return 500 and .rb is outside the indexer whitelist` |

The dataset exposes **only a `train` split** for every config, so `CosQA=test` but all three
`CodeEditSearchRetrieval-*=train` — the usual `test -> valid -> train` preference has nothing else to fall back on.

### 2.3 The adaptive cutoff: why this batch runs with it disabled

`AdaptiveCutoffConfig::default()` (`rust-core/src/services/hybrid_search.rs`) is
`enabled = true, abs_floor = 0.2, rel_ratio = 0.55, rel_ratio_rrf = 0.40, gap_min = 0.30, min_keep = 1`.
`adaptive_cutoff()` normalises the fused score into `rel ∈ [0, 1]` and then applies **three cascaded gates** as a
prefix truncation: (1) an absolute floor `rel > abs_floor`, which is a **hard gate**; (2) a relative ratio
`rel >= rel_ratio × rel[0]` (or `rel_ratio_rrf` in the RRF score domain); (3) a gap detector on adjacent jumps.

The final length is `final_len = len_gap.max(cfg.min_keep.min(len_abs))`, and because gate 1 is a hard gate,
**`min_keep` cannot rescue anything the absolute floor has already rejected**. On short queries the normalised
score collapses right after the top hit — everything past `top1` falls below `rel = 0.2` — so `len_abs` collapses
to 1 and the result list is trimmed to **one** item. A short query is therefore scored against a single-candidate
list: `nDCG@10`, `recall@10` and `MRR@10` all reduce to "is rank 1 correct", so the three numbers necessarily
coincide, and any query whose one surviving item is not the gold document scores `0.0000` on all of them.

There is **no CLI flag and no environment variable** for it: the config loader reads a single path,
`~/.codeseek/config.json` (`rust-core/src/config.rs`, `codeseek_dir()`), so the only override is to write
`{"index": {"hybrid": {"cutoff": {"enabled": false}}}}` into that file (merge, do not overwrite — the `embedding`
section must survive).

With `RUST_LOG=debug` the binary logs which branch it took, in the shape
`adaptive cutoff disabled: <before> -> <after> items (truncated only by k_final=<n>)`. The line observed for this
batch was **`adaptive cutoff disabled: 20 -> 20 items`**, which pins the behaviour to the early-return path: no
gate ran and the list was only truncated by `k_final = 20`. With the cutoff left at its default the emitted count
instead collapses to about one item, which is what the `final_len` formula above predicts once the absolute floor
rejects everything past the top hit.

## 3. Evaluation conditions

| item | value |
| --- | --- |
| evaluation condition | **`index.hybrid.cutoff.enabled = false`** (all other cutoff sub-fields keep their defaults) |
| data subset protocol | 4 tasks × 25 queries on average = **100 queries**; pooled corpus **300 documents per task** = **1,200 documents** total |
| tasks | CosQA (40 queries), `CodeEditSearchRetrieval-python` (20), `-go` (20), `-typescript` (20) |
| dataset access | HuggingFace **datasets-server JSON REST API**, `100` rows per `/rows` page, through the local proxy `http://127.0.0.1:7897` (a direct connection to HF is not reachable from this machine) |
| dataset access (typescript) | **parquet, not REST**: `datasets-server` `/rows` answers **500** for all three `typescript-*` configs, so this language is read from the official parquet files through `--parquet-cache ~/.cache/huggingface/hub` (`meta.client_stats.parquet_pages = 139`, `meta.data_sources.rest_requests = 12`) |
| qrels splits | `CosQA=test`, `CodeEditSearchRetrieval-python=train`, `-go=train`, `-typescript=train` |
| primary metric | **nDCG@10**; overall = **arithmetic mean of per-task means** (MTEB `Mean(Task)`); `search_limit=20`, `k=10`, `seed=42` |
| sampling | `sampling_fingerprint = 94e480122f909def` (deterministic; re-running `prepare` with the same seed reproduces the same 100 queries and the same 300-document pools) |
| retrieval mode | `hybrid: 100` (no `name_fallback`, no `unknown_empty`) |
| binary under test | `rust-core/target/release/codeseek`, `sha256` prefix **`b1532bb1b43e3d65`**, `size_bytes=60600320`, mtime `2026-11-12T08:06:16.385072+00:00` |
| embedding config | `Qwen/Qwen3-Embedding-4B`, `api_token` length **51** (non-empty, so hybrid mode is genuinely enabled) |
| run scale | `generated_at = 2026-11-13T02:25:45+00:00`, wall **75.2 s**, `concurrency = 4`, `init_failures = []` |
| per-query search time | mean **2.90 s**, median **2.65 s**, p90 **3.77 s**, max **5.30 s**; 289.5 s of search in total / 4 concurrent workers ≈ the 75.2 s wall clock |
| `codeseek init` | 4 invocations (one per corpus), measured **0.3 s / 0.3 s / 0.3 s / 0.4 s**; the index persisted between invocations, so each init is a fast no-op |
| reranker | `enabled`, `top_n = 5`, but `derived_top_n = max(top_n, final_budget) = 20` — recorded as an **uncontrolled variable** |
| warnings | `[]` (no `prepare` warning, no scoring warning) |

## 4. Key insights

### 4.1 Ranking is near-optimal once the gold document is recalled

Conditional nDCG@10 is **0.9188** and conditional MRR@10 is **0.8922**, i.e. the gold document sits at mean rank
**1.12** whenever it is retrieved at all. CosQA shows the same shape in a single row: `recall@10 = 1.0000` with
`nDCG@10 = 0.9690`, which is only possible if nearly every hit is at position 1. **There is very little left to
win in the ranker.**

### 4.2 The bottleneck is recall, not ranking

Of the 41.42 points missing from a perfect score, **36.25 are recall failures and only 5.17 are ordering loss**
(Section 1.3). The strongest corroboration is that **`recall@10 == recall@20 == 0.6375`**: not a single gold
document lands in positions 11–20. The distribution is **bimodal** — a gold document is either inside the Top-10
or nowhere in the returned list at all. That is the signature of a **candidate-generation** problem (the dense and
BM25 channels never surface the right document), not of a document that was found but ranked too low.

### 4.3 CodeEditSearchRetrieval's low scores are a task-design problem, not a retrieval problem

The corpus of this task family is **not source code**: the raw `code` field is a **unified diff** — measured
**1,000 of 1,000** rows are diffs — and this suite normalises each one to post-image code before materialization
(`TaskSpec.diff_postimage = True`). That normalisation is not cosmetic: writing **30 raw diffs** out as `.py`
only lets codeseek index **9 files / 11 symbols**; after post-image normalisation the same 30 diffs become
**17 files / 80 symbols** (`mtebcode_eval/data.py`, `diff_postimage` docstring).

Even after normalisation the corpus is thin, and measurably thinner than CosQA's. Over the 300 pooled documents
per task, the share of documents that contain a **line-initial declaration of the language's primary definition
form** (`def`/`class` for python, `func` for go, `function`/`class`/an arrow-function assignment for typescript,
`def`/`class` for CosQA) is:

| task | documents with a definition | corpus shape |
| --- | --- | --- |
| CosQA | **299/300 (99.7%)** | short python snippets, essentially every one of them a definition |
| CodeEditSearchRetrieval-python | **173/300 (57.7%)** | post-image fragments of a diff |
| CodeEditSearchRetrieval-go | **187/300 (62.3%)** | post-image fragments of a diff |
| CodeEditSearchRetrieval-typescript | **100/300 (33.3%)** | post-image fragments of a diff |

These four numbers are measured with **one** definition — line-initial only — so they are comparable with each
other.

The queries, meanwhile, are **commit-message shaped** (`Test that Add actually works.`, `Use absolute value for
NTP drift`): they describe the *intent* of a change, while the document is a fragment of the *result* of that
change and contains neither the intent nor a complete function. The consequences are visible in the ordering of
the metrics: the worst 10 queries all come from this task family and **all of them returned the full 20 results**
(see Section 4 of `2026-11-13-report.md`) — so these are genuine recall failures, but failures caused by
**task unretrievability**, not by a broken retriever.

Note that definition density **does not order the scores monotonically**: python has the highest CodeEdit density
(57.7%) yet the lowest CodeEdit score (**0.4131**), while typescript has the lowest density (33.3%) and scores
**0.4715**. Density is a partial explanation, not the whole one; the dominant factor is the semantic gap between a
commit message and a diff fragment, which is what pulls the entire family far below CosQA (recall@10 `1.0000`).

## 5. Known limitations and next steps

- **Not comparable with the MTEB(Code, v1) leaderboard.** The corpus is pooled down to 300 documents per task,
  only 25 queries per task are sampled, and codeseek is a local code search engine rather than an embedding
  model. This archive is only valid for **longitudinal self-comparison** of successive codeseek builds.
- **A single gold document per query makes nDCG@10 and MRR@10 monotone transforms of the same rank**
  (`nDCG = 1 / log2(rank + 1)`, `MRR = 1 / rank`). The only genuinely independent signal in this table is
  **recall**. Decoupling ranking quality from recall would require multi-gold tasks or graded relevance.
- **`recall@10 == recall@20`** shows the current search budget of 20 is sufficient, but it also means **nothing
  is being measured about positions 11–20** — the protocol cannot distinguish "ranked 11th" from "not retrieved".
- **Pooling changes difficulty.** A 300-document pool is far smaller than the real corpus (the Hub reports
  **20,604** corpus rows for CosQA and **280,310** for CodeSearchNet-python), so absolute recall here is
  **systematically higher** than it would be on the full corpus. Magnitudes are comparable across batches, not
  against other harnesses.
- **CodeEditSearchRetrieval only exposes a `train` split**, java has **no qrels at all**, and ruby returns 500, so
  that task family is **incompletely covered** and is represented by three languages out of six. The current
  three are `python`, `go` and `typescript`.
- **The reranker is an uncontrolled variable.** It was `enabled` with `top_n = 5`, but
  `derived_top_n = max(top_n, final_budget) = 20`, so its real effect on these short queries was **not measured**.
  A reranker-off run would be needed to attribute any of the ordering quality in Section 4.1 to it.
- **The cutoff configuration is part of the result.** The archived numbers only hold with
  `index.hybrid.cutoff.enabled = false`; the default trims short queries to about one result, which collapses
  `nDCG@10` / `recall@10` / `MRR@10` onto the same value. Section 5 of the rendered report prints this
  precondition above the commands.
- **Long-query sensitivity is a query-side property, not noise.** The screening probe truncated one
  AppsRetrieval query to its 152-character first line and recovered 3 results, i.e. codeseek (or the embedding
  endpoint) has a practical usability ceiling on very long queries. That is a **product defect**, and it is why
  long-query tasks are excluded from the task set rather than merely scoring low.
- **Next steps, in priority order.** (a) Attack recall: check whether `vector_top_k` / `bm25_top_k` (default 100
  each) are being diluted by the pooled corpus, and whether the embedding instruction template suits
  commit-message-shaped queries. (b) Replace the CodeEditSearch family with a task whose corpus is **real source
  files**, so that the metric stops measuring diff fragments. (c) Run a production-side sweep over
  `abs_floor × min_keep` and pick a precision/recall recommendation for the default config, since the current
  default (`abs_floor = 0.2`, `min_keep = 1`) silently discards most of the candidate list on short queries.

## 6. Reproduction

**Precondition.** The numbers above require `index.hybrid.cutoff.enabled = false` in `~/.codeseek/config.json`
(merge into the existing JSON, do not overwrite the `embedding` section), and that config must be restored
afterwards. Section 5 of `2026-11-13-report.md` prints the same warning above the commands.

```bash
cd evals/mtebcode

# 0) offline self-test (no network, no binary needed)
python3 -m mtebcode_eval selftest

# 1) fetch the dataset, sample the queries, pool the corpus, materialize (the real parameters of this batch)
python3 -m mtebcode_eval prepare \
  --workdir /tmp/mtebcode-ts \
  --tasks CosQA,CodeEditSearchRetrieval-python,CodeEditSearchRetrieval-go,CodeEditSearchRetrieval-typescript \
  --limit-queries 20 \
  --corpus-size 300 \
  --seed 42 \
  --parquet-cache ~/.cache/huggingface/hub \
  --proxy http://127.0.0.1:7897

# 2) run retrieval and write the report
python3 -m mtebcode_eval run \
  --workdir /tmp/mtebcode-ts \
  --limit 20 \
  --k 10 \
  --concurrency 4

# 3) re-score only (no retrieval re-run)
python3 -m mtebcode_eval score \
  --workdir /tmp/mtebcode-ts \
  --limit 20 \
  --k 10
```

`--parquet-cache` is **required** for `CodeEditSearchRetrieval-typescript`: `datasets-server` `/rows` answers
`500` for all three of its configs, so the rows are read from the official parquet files in the local HF hub
cache instead of the REST API.

**Provenance.** Binary under test `rust-core/target/release/codeseek`, `sha256` prefix `b1532bb1b43e3d65`
(`size_bytes = 60600320`, mtime `2026-11-12T08:06:16.385072+00:00`). `2026-11-13-report.md` is rendered from
`2026-11-13-report.json` by `render_markdown()`, and `md == render_markdown(json)` was verified to be **`True`**
byte for byte. `--limit-queries 20` is the global default; CosQA's quota of 40 is set in the task registry
(`mtebcode_eval/data.py`, `TaskSpec.query_quota`), which is what brings the batch to 40 + 3 × 20 = 100 queries.

For the data protocol, the sampling scheme and the scoring rules see the parent [`../README.md`](../README.md).
