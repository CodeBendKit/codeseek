# MTEB(Code, v1) × codeseek measured results archive

> **Primary result: the `2026-11-16` report, mean nDCG@10 = 0.9182** (101 tasks / 3040 queries), binary under test
> `sha256` prefix `d14604f6583c55c5`, evaluation condition **`index.hybrid.cutoff.enabled = false`**.
> The body of this document (key takeaways / task and language selection / evaluation conditions / key insights /
> known limitations) **refers to that batch throughout**.

This archives codeseek's end-to-end retrieval results on **101 tasks / 3040 queries** of MTEB(Code, v1)-style data
(measured, not estimated and not extrapolated from a sample). It holds a single batch: the **2026-11-16 result
(mean nDCG@10 = 0.9182)** — 100 `CodeSearchNetRetrieval-<lang>-sNN` shards (go 34 / java 33 / python 33) plus
CosQA, over 3 shared per-language corpora of 1,000 real source files each and one 300-document CosQA pool.

| file | description |
| --- | --- |
| `2026-11-16-report.md` | **Primary result**: human-readable report (summary / by language / retrieval-mode statistics / per-task breakdown / top-10 worst queries / reproduction commands) |
| `2026-11-16-report.json` | **Primary result**: machine-readable detail (`meta` / `aggregate` / `per_query` / `failure_top` / `warnings`) |
| `README.md` | This file: archive index, task and language selection, evaluation conditions, insights, limitations |

**Provenance**: `2026-11-16-report.md` is produced by `render_markdown()` in `mtebcode_eval/report.py` from
`2026-11-16-report.json`; **no number was edited by hand**, and `md == render_markdown(json)` was verified
**byte for byte** (`True`, 35,717 characters on both sides). Measured values of the archived files:

| file | bytes | md5 |
| --- | --- | --- |
| `2026-11-16-report.md` | 35823 | `86a7bacee240b38f8d03892b9453c8f9` |
| `2026-11-16-report.json` | 6030638 | `b22dfdc37ba248f1b7b8bd2c8d4f0e5d` |

The `.json` file was **never rewritten** after the run: only the `.md` was rendered from that same `.json`.
Every statistic below is read from the `.json`.

**Batch**: 2026-11-16 — 101 tasks, 3040 queries, 3300 documents indexed, `index.hybrid.cutoff.enabled = false`,
`k = 10`, `search_limit = 20`, `seed = 42`, `sampling_fingerprint = 9f6e9f7d9d0dd280`.

Per-group query counts: the 100 CodeSearchNet shards carry **3000** queries (1,000 per language, deterministically
sliced), CosQA carries **40** (its `query_quota` in the task registry, above the global `--limit-queries 30`).
The overall number is the **arithmetic mean of the 101 per-task means** (MTEB `Mean(Task)`), **not** a pooled mean
over 3040 queries:

`(100 × 0.9177619800 + 0.9598197315) / 101 = 0.9181783934` -> `0.9182`

> **History.** The retired `CodeEditSearchRetrieval` task family (and the earlier CoIR-version
> `CodeSearchNetRetrieval` single task) has been removed from this archive along with its `2026-11-13` batch —
> its corpus was post-image diff fragments rather than real source code; the old files remain reachable through
> git history only.

## 1. Key takeaways

**Overall Mean nDCG@10 = 0.9182** across 101 tasks and 3040 queries
(recall@10 **0.9720**, MRR@10 **0.9004**, recall@20 **0.9819**):

| scope | tasks | queries | nDCG@10 | recall@10 | MRR@10 | recall@20 (search limit) |
| --- | --- | --- | --- | --- | --- | --- |
| **Overall `Mean(Task)`** | **101** | **3040** | **0.9182** | **0.9720** | **0.9004** | **0.9819** |
| `CodeSearchNetRetrieval` (100 shards) | 100 | 3000 | 0.9178 | — | — | — |
| CosQA | 1 | 40 | 0.9598 | 1.0000 | 0.9458 | 1.0000 |

Four decimals are kept because nDCG differences are much finer than percentage-point accuracy.
The 100-shard row is the mean of the 100 shard means measured in the shard-only round (`0.9177619800`); merging
CosQA into the same task mean is what turns it into the 101-task `0.9182` above. The `—` cells are not part of
the archived per-group table: the shard round is archived as a single nDCG figure, and re-deriving a recall@10 for
it from the merged report would mix two aggregation scopes.

### By language (`mean_of_task_means`)

| language | tasks | nDCG@10 | recall@10 | MRR@10 | queries (scored/total) |
| --- | --- | --- | --- | --- | --- |
| go | 34 | 0.9448 | 0.9870 | 0.9306 | 1000/1000 |
| java | 33 | 0.9101 | 0.9607 | 0.8935 | 1000/1000 |
| python | 34 | 0.8994 | 0.9681 | 0.8769 | 1040/1040 |

> **Aggregation scope of this table is `mean_of_task_means`**: each language row is the arithmetic mean of that
> language's *per-shard task means*, so a language with 34 shards does not outvote one with 33. The alternative
> **query-level** scope (mean over that language's queries) also exists in the archived JSON and is *not* used
> here: `go 0.944746 / java 0.910439 / python 0.899749`. The two scopes are **never mixed** in this document.

> **The python row contains 34 tasks = 33 CSN python shards + CosQA.** CosQA's `language` is `python` in the task
> registry, so it is folded into this group — which is why python carries **1040** queries while go and java carry
> 1000 each. Even with CosQA (0.9598, i.e. above the python average) merged in, python stays the lowest group.

### 1.1 Shard distribution

| item | value |
| --- | --- |
| worst shard | `CodeSearchNetRetrieval-python-s22` — **0.7523** |
| 2nd / 3rd worst | `python-s30` **0.7949**, `java-s27` **0.7971** |
| perfect shards (nDCG@10 = `1.0000`) | `go-s26`, `go-s33`, `java-s09` |
| shards with an empty result list | **0** — every one of the 100 shards has `mean_returned = 20.0`, `mean_mapped = 20.0`, `with_errors = 0` |
| queries per shard | go: 14 shards × 30 + 20 shards × 29 = 1000; java and python: 10 shards × 31 + 23 shards × 30 = 1000 each |

Four shards actually tie at the ceiling: **`go-s07` also measures exactly `1.0000`**, in addition to the three
named above (verified against `aggregate.by_task` in the archived JSON).

A shard is ~30 queries, so a single query is worth ~3.3 nDCG points **inside one shard**: the 0.7523–1.0000 spread
is not evidence about language difficulty, it is sampling noise at n≈30. Only the aggregated numbers are signals.

### 1.2 Retrieval mode statistics

| mode | queries | share | meaning |
| --- | --- | --- | --- |
| `hybrid` | **3040** | 100.0% | true semantic retrieval (LanceDB + BM25, results carry a `code_block`) |
| `name_fallback` | 0 | 0.0% | degradation to function-name substring matching, used when `embedding.api_token` is absent |
| `unknown_empty` | 0 | 0.0% | empty or unparsable search results, impossible to judge |

All 101 tasks report a single mode (`hybrid`); `mean_returned = 20.0` / `mean_mapped = 20.0` /
`mean_unmapped = 0.0` hold for CosQA and for every shard, and `with_errors = 0` over the whole batch.

> ⚠️ **`mode = hybrid` does not prove the vector channel was available.** The runner never exposes a
> `vector_available` flag: when `EmbeddingService` fails to initialise, `Commands::Search` sets
> `search_degraded = true` and can still answer from the name/graph path, so the `mode` label alone is not
> evidence. The vector channel in this batch was verified **out of band**: `lancedb` directories of 10–12 MB for
> the CSN corpora and 3.1 MB for CosQA, `embedding_hashes` row counts equal to the LanceDB row counts, and a
> `csn-go` data file of `10,703,914 B ≈ 1000 × 2560 × 4 B`. See limitation 5 in Section 5.

### 1.3 Failure decomposition

| component | value | meaning |
| --- | --- | --- |
| `1 − recall@10` | **0.0280** | outright recall failure (the gold document never reaches Top-10) |
| `recall@10 − nDCG@10` | **0.0538** | ordering loss **after** the gold document was retrieved |
| `nDCG@10` | 0.9182 | the two components above, subtracted from 1.0 |

Of the **8.18 points** missing at the top of the scale, **2.80 are recall failures and 5.38 are ordering loss**
(`0.0280 + 0.0538 = 0.0818 = 1 − 0.9182`).

> The split above is computed on the 4-decimal values printed in this document (that is what makes the identity
> hold exactly). On the unrounded means the two terms are `0.0279514826` and `0.0538701240`, summing to
> `0.0818216066 = 1 − 0.9181783934`.

**This is a structurally different failure profile from the retired diff-fragment batch**, where recall owned
almost all of the loss. Here **ordering loss (5.38) is nearly twice the recall loss (2.80)**: the bottleneck has
moved from "the right document is not in the candidate list" to "the right document is found but not ranked
first". That shift is the single most important observation in this report (Section 4.2).

## 2. Task and language selection

### 2.1 The data shape of `mteb/CodeSearchNetRetrieval`

| property | value |
| --- | --- |
| configs | **18** = 6 languages × 3 (`<lang>-corpus`, `<lang>-queries`, `<lang>-qrels`) |
| rows | **1,000 per config** |
| splits | **only `test`** — there is no `train`/`valid` to fall back on |
| corpus content | **real source code**, indentation and newlines preserved |
| queries | the very **docstrings that were removed from the corpus** (blanked on one side, moved to the other) |
| qrels | `query-id` / `corpus-id` / `score`, **1 query ↔ 1 corpus document**, `score` always `1` |
| languages present | go, java, javascript, php, python, ruby |

### 2.2 How the 100 shards are cut

- Each language's 1,000 queries are sorted, then split by `shard_query_ids()` into **deterministic contiguous,
  mutually exclusive slices** whose union is the whole 1,000: **go 34 shards, java 33, python 33** (100 total).
  Shard sizes therefore differ by at most one query (go 29–30, java/python 30–31).
- Each language's **full 1,000-document corpus** is materialized **once** and shared by every shard of that
  language (`TaskSpec.corpus_key` + `full_corpus=True`). That is why 100 tasks need only **3 `codeseek init`
  runs**, and why `--corpus-size` is ignored for them.
- Every shard sees byte-identical `docs/` and `docmap.json`, so shard-to-shard differences come only from the
  query slice, never from a different index.

### 2.3 CosQA — the 101st task

| property | value |
| --- | --- |
| dataset | `CoIR-Retrieval/cosqa` (corpus 20,604 rows), qrels split `test` |
| sample | **40** queries (`TaskSpec.query_quota = 40`, above the global `--limit-queries 30`), 300-document pool (gold 40 ∪ distractors) |
| documents | `.py` snippets; unlike the CSN shards this task **is** pooled, so it needs its own index |
| measured | nDCG@10 **0.9598**, recall@10 **1.0000**, MRR@10 **0.9458**, recall@20 **1.0000**, `hybrid × 40`, `mean_returned = 20.0`, `mean_mapped = 20.0`, `mean_unmapped = 0.0`, `with_errors = 0` |
| per-query score | **36/40 at 1.0000**; the three worst are `0.5000` "python cast true or false as numbers", `0.6309` "ply python expression evaluator", `0.6309` "filling null value sin data frame in python" |
| per-query latency | min **2.21 s**, median **2.65 s**, mean **2.77 s**, max **4.29 s** |

### 2.4 Language and task-family exclusions

| dataset / family | verdict |
| --- | --- |
| `javascript`, `php` | **excluded by requirement** — not evaluated in this archive |
| `ruby` | **excluded**: `.rb` is **not** in codeseek's index whitelist (`rust-core/src/codegraph/parser.rs`, `is_supported_file`) and tree-sitter has no Ruby parser here, so the corpus would be silently skipped and the index would hold **0 files** |
| the retired diff-fragment family (python / go / typescript / java / ruby) | **removed from the registry and from this archive** (see the history note at the top): its corpus rows were **unified-diff post-image fragments** rather than real source files, it exposed **only a `train` split**, java had **no qrels** and ruby's configs answered HTTP 500 |
| CoIR-version `CodeSearchNetRetrieval` (single task) | **removed**: its corpus `text` field is the **docstring**, not the code — `codeseek init` indexed only **1 file / 3 symbols** |
| `SyntheticText2SQL`, `CodeFeedbackST`, `CodeFeedbackMT`, `AppsRetrieval`, `StackOverflowQA`, `CodeTransOceanContest`, `CodeTransOceanDL` | **screened out and kept in the registry with an `excluded_reason`** (`.sql` outside the index whitelist; query medians 540 / 3,571 / 1,316 / 883 / 547 / 1,211 chars) |

## 3. Evaluation conditions

| item | value |
| --- | --- |
| evaluation condition | **`index.hybrid.cutoff.enabled = false`** (all other cutoff sub-fields keep their defaults, `abs_floor = 0.2`) |
| benchmark / command | `MTEB(Code, v1)` / `run` |
| batch | `generated_at = 2026-11-16T07:52:58+00:00`, `workdir = /tmp/mtebcode-smoke` |
| tasks | **101** = 100 `CodeSearchNetRetrieval-<lang>-sNN` shards + CosQA; `n_tasks_scored = 101`, `n_queries_scored = 3040/3040`, `n_queries_skipped = 0` |
| queries / documents | **3040** queries over **3300** indexed documents (3 × 1000 shard-corpus files + 300 CosQA pool files) |
| sampling | `seed = 42`, `corpus_size = 300`, `limit_queries = 30` (CosQA overrides it to 40 via `query_quota`), `sampling_fingerprint = 9f6e9f7d9d0dd280` |
| metrics | primary **nDCG@10**, overall = arithmetic mean of per-task means (`mean_of_task_means`); `k = 10`, `search_limit = 20` (used by `recall@k`) |
| run scale | `concurrency = 8`, `elapsed_s = 52.0` (CosQA incremental round), `init_failures = []`, `prepare_warnings = []`, `warnings = []` |
| retrieval mode | `mode_counts = {"hybrid": 3040}`, `mode_ratio = {"hybrid": 1.0, "name_fallback": 0.0, "unknown_empty": 0.0}` |
| binary under test | `rust-core/target/release/codeseek`, `sha256` prefix **`d14604f6583c55c5`**, `size_bytes = 60600272`, mtime `2026-11-16T06:25:00.219497+00:00` |
| embedding config | `Qwen/Qwen3-Embedding-4B`, `api_token` length **51** (non-empty, so hybrid retrieval is genuinely enabled) |
| dataset access | HuggingFace `datasets-server` JSON REST API through the local proxy `http://127.0.0.1:7897`, 100 rows/page, page cache + retries with backoff |
| `codeseek init` (100-shard round) | 3 invocations, one per shared language corpus: **go 97.6 s / java 363.3 s / python 327.0 s** (788 s total) |
| retrieval (100-shard round) | 3000 queries in ≈ **10.3 min**; total wall clock **23.4 min** |
| CosQA incremental round | prepare **31 s**, run **52.0 s** (of which `codeseek init` **30.1 s**), end-to-end ≈ **2 min** |
| index size (LanceDB) | `csn-go` **10 MB**, `csn-java` **10 MB**, `csn-python` **12 MB**, `cosqa` **3.1 MB**; `csn-go` data file `10,703,914 B ≈ 1000 × 2560 × 4 B`, `cosqa` data file `3,232,794 B = 300 × 2560 × 4 B` |

## 4. Key insights

### 4.1 The score is a property of the corpus, not of a trick

The measured change from the retired diff-fragment batch (mean nDCG@10 **0.5858**) to this one (**0.9182**) tracks
exactly one structural change: the corpus became **real source code**. A code chunk cut out of a real file is a
self-contained, semantically indexed unit; a post-image fragment of a unified diff is not — the same 30 raw diffs
index to **9 files / 11 symbols**, and even after post-image normalisation only **17 files / 80 symbols**.
**The two numbers are not a strict improvement measurement**: the task set, the corpus, the queries and the
shard structure all changed, so 0.5858 -> 0.9182 must be read as "the protocol now measures something
retrievable", not as a 33-point engineering gain.

### 4.2 The remaining loss has flipped from recall to ranking

At **0.9182** the missing 8.18 points decompose into **2.80 recall** and **5.38 ordering** (Section 1.3). In the
retired batch the split was the opposite extreme (**36.25 recall / 5.17 ordering**). Two consequences:

- **Candidate generation is healthy.** 97.2% of gold documents land in the Top-10 and `recall@20 = 0.9819` — the
  channels almost always surface the right file.
- **The ranker is now the binding constraint.** Nearly two thirds of the residual loss is "found, but not first".
  Any further work that only widens the candidate pool has little headroom left; precision at rank 1 is where the
  points are.

### 4.3 Language differences are small and do not order the way intuition suggests

go **0.9448** / java **0.9101** / python **0.8994** — a **4.5-point spread** across three languages with thousands
of queries each. Compare that with the retired batch, where differences *between task families* reached 55 points.
The reading is that this task family is **uniformly solvable for codeseek**: real-source retrieval transfers
across go/java/python, and language choice is a second-order effect rather than the dominant variable.
Python is the weakest language **even after CosQA (0.9598) was merged into it**, so the gap is not a pooling
artefact of the added task.

### 4.4 CosQA is the "recall saturated, ranking slightly degraded" case

`recall@10 = 1.0000` with `nDCG@10 = 0.9598`: **all 40 gold documents were retrieved and 36 of them at rank 1**.
The whole deficit comes from 4 queries whose gold document is found but ranked lower (worst `0.5000`, two at
`0.6309`). This is the cleanest illustration of Section 4.2 at single-task scale — there is nothing left to fix in
recall, and the outstanding points are pure ordering.

### 4.5 Shard-level numbers are noise; aggregate scopes are the signal

The 100 shards span **0.7523 – 1.0000**, but each holds only ~30 queries, so one query moves a shard by ~3.3
points and a single shard cannot support a conclusion about a language, a shard or a build. The mean over the 100
shard means (0.9178) and the 101-task mean (0.9182) are the only comparable quantities — and the **"By language"
table must be read in the `mean_of_task_means` scope** (Section 1), because `Mean(Task)` deliberately gives a
30-query shard the same weight as a 31-query one.

## 5. Known limitations and next steps

- **Not comparable with the MTEB(Code, v1) leaderboard.** The corpus is pooled down to 300 documents for CosQA
  (the CSN shards index their language's whole 1,000-document sample), and codeseek is a local retrieval engine,
  not an embedding model. This archive is valid for **longitudinal self-comparison** of codeseek builds only.
- **A single gold document per query makes nDCG@10 and MRR@10 monotone transforms of the same rank**
  (`nDCG = 1 / log2(rank + 1)`, `MRR = 1 / rank`). The only independent signal here is **recall**.
- **`recall@10 = 0.9720` vs `recall@20 = 0.9819`**: the 20-result budget is essentially sufficient, but the small
  gap also means positions 11–20 contribute almost nothing, so the protocol can barely distinguish "ranked 11th"
  from "never retrieved".
- **The cutoff configuration is part of the result.** `index.hybrid.cutoff.enabled` must be `false`; the default
  (`abs_floor = 0.2`, `min_keep = 1`) trims a short query to about one result and collapses `nDCG@10`,
  `recall@10`, `MRR@10` onto the same value. A number without its cutoff state is not interpretable.
- **`mode = hybrid` is not evidence that the vector channel worked.** The runner exposes no `vector_available`
  flag, and `search_degraded = true` can still answer through the name path. This batch was verified out of band
  (LanceDB sizes + `embedding_hashes` row counts); the label itself proves nothing.
- **Unfixed defect — `embedding_hashes.json` over-reports.** When the vector write fails, the file is still
  written full (1,000 entries), `init` "succeeds" in ~2 s, no vectors are stored and no warning is emitted; an
  empty LanceDB table cannot self-heal. This batch only reached its correct numbers **after manually deleting
  polluted index directories**; an earlier run with a poisoned index produced a fake 0.3945 with
  `unknown_empty = 55.1%`.
- **`codeseek init` cost is uneven and unexplained**: java **363.3 s** and python **327.0 s** against go
  **97.6 s** for the same 1,000-document corpus size, with no 429 or retry lines in the log. Correctness is
  unaffected (hash rows match LanceDB rows); it is an embedding-throughput or network-path effect.
- **Pooling changes difficulty.** CosQA's 300-document pool is far smaller than its real corpus (20,604 rows), so
  its absolute recall is **systematically higher** than on the full corpus.
- **Next steps, in priority order.**
  (a) Fix the `embedding_hashes` over-reporting and expose `vector_available` in the runner, so a degraded index
  can never look like a good score;
  (b) sweep the `abs_floor × min_keep` defaults in `rust-core/src/services/hybrid_search.rs` and pick a
  precision/recall recommendation for the shipped default;
  (c) to cover `javascript` / `php` / `ruby`, first add the corresponding tree-sitter parsers and whitelist
  entries to `rust-core` — it is an indexer gap, not an evaluation gap;
  (d) add multi-gold or graded-relevance tasks to decouple recall from ranking, since ranking is now the dominant
  loss term.

## 6. Reproduction

**Precondition.** These numbers require `index.hybrid.cutoff.enabled = false` in `~/.codeseek/config.json`
(merge the snippet below, never overwrite, so the `embedding` section holding `api_token` survives) and the file
must be restored afterwards. There is no CLI flag and no environment variable for the cutoff.

```json
{"index": {"hybrid": {"cutoff": {"enabled": false}}}}
```

```bash
cd evals/mtebcode

# 0) offline self-test (no network, no binary needed)
python3 -m mtebcode_eval selftest

# 1) fetch + shard + materialize  (`--tasks csn` = alias for all 100 CodeSearchNet shards; `CosQA` is the 101st)
python3 -m mtebcode_eval prepare \
  --workdir /tmp/mtebcode-smoke \
  --tasks csn,CosQA \
  --limit-queries 30 \
  --corpus-size 300 \
  --seed 42 \
  --proxy http://127.0.0.1:7897

# 2) retrieval + scoring + report
python3 -m mtebcode_eval run \
  --workdir /tmp/mtebcode-smoke \
  --limit 20 \
  --k 10 \
  --concurrency 8

# 3) re-score only (no retrieval re-run)
python3 -m mtebcode_eval score --workdir /tmp/mtebcode-smoke --limit 20 --k 10
```

Notes that make the batch reproducible:

- **`--tasks csn`** is an alias that expands to the 100 `CodeSearchNetRetrieval-<lang>-sNN` task names; the CSN
  shards ignore `--limit-queries` / `--corpus-size` (each takes its own slice of the 1,000 queries and the whole
  1,000-document corpus of its language), so those two flags only shape **CosQA**.
- **CosQA's quota of 40** comes from `TaskSpec.query_quota` in the registry (`mtebcode_eval/data.py`), which
  overrides the global `--limit-queries 30`; that is what brings the batch from 3000 to **3040** queries.
- `--concurrency 8`, `--limit 20`, `--k 10` are the measured values (`meta.concurrency`, `meta.search_limit`,
  `meta.k`).
- The 100-shard round and the CosQA round were prepared/run separately and then merged into one report; the
  archived `meta.elapsed_s = 52.0` is the CosQA incremental run (its `prepare` took 31 s), while the 3000-query
  shard round took ~10.3 min of retrieval and 23.4 min wall clock including the three `codeseek init` calls.

**Provenance.** Binary under test `rust-core/target/release/codeseek`, `sha256` prefix `d14604f6583c55c5`
(`size_bytes = 60600272`, mtime `2026-11-16T06:25:00.219497+00:00`), embedding `Qwen/Qwen3-Embedding-4B` with a
51-character token. `2026-11-16-report.md` is rendered from `2026-11-16-report.json` by `render_markdown()`, and
`md == render_markdown(json)` was verified **`True`** byte for byte.

For the data protocol, the sampling scheme and the scoring rules see the parent [`../README.md`](../README.md).
