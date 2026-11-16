# RepoQA measured results archive

> **Primary result (current): the 2026-11-12 (b) report, overall 84.0% (84/100)**, binary under test `sha256` prefix
> `b1532bb1b43e3d65`.
> The body of this document (rejected approaches that were measured / key takeaways / evaluation conditions / three
> key insights / known limitations) **refers to that batch throughout**.
>
> **Historical comparison (previous-round baseline): the 2026-11-12 report (first batch), overall 82.0% (82/100)**,
> `sha256` prefix `38edff1bb1b1d1cd`, corresponding to the rust `mod_item` fix.
> **Historical comparison (pre-fix baseline): the 2026-11-11 report, overall 74.0% (74/100)**, `sha256` prefix `5972636dc15431ee`.
> Apart from the sections that are explicitly labelled as a baseline, the body no longer cites numbers from these two batches.

This archives codeseek's end-to-end results on the RepoQA **100-task subset** (measured, not estimated and not
extrapolated from a sample). It currently holds **three batches**: the 2026-11-12 (b)
**rank-time enclosing-candidate demotion result (primary result, 84.0%)**, the 2026-11-12
**previous-round baseline (82.0%, rust `mod_item` fix)** and the 2026-11-11
**pre-fix baseline (74.0%, kept for historical comparison only)**.

| file | description |
| --- | --- |
| `2026-11-12b-report.md` | **Primary result**: human-readable report (overall / per language / per repo / top-10 failures / reproduction commands), overall 84.0%, BLEU backend = `nltk` |
| `2026-11-12b-report.json` | **Primary result**: machine-readable detail (`meta` / `aggregate` / `by_repo` / per-task `per_task` / `failure_top`) |
| `2026-11-12-report.md` | **Previous-round baseline**: report after the rust `mod_item` fix, overall 82.0%, BLEU backend = `nltk` |
| `2026-11-12-report.json` | Same as above, machine-readable detail |
| `2026-11-11-report.md` | **Pre-fix baseline (historical comparison)**: same protocol, human-readable report, overall 74.0%, BLEU backend = `nltk` |
| `2026-11-11-report.json` | **Pre-fix baseline**: machine-readable detail |
| `2026-11-11-report.builtin.md` | Baseline copy produced by the built-in BLEU replica (`backend=builtin`), used to cross-check against nltk |
| `2026-11-11-report.builtin.json` | Same as above, machine-readable version |

All `.md` / `.json` files were copied verbatim from the same-named artifacts under `/tmp/repoqa-eval/` (the default
`--workdir`) and only renamed with a date prefix; the four `.md` files were **then re-rendered once in English** by
`render_markdown()` in `repoqa_eval/report.py` from their companion `.json`, with
**every statistic and conclusion unchanged** -- only the prose language moved. Two `.json` files received
**prose-only** edits; the exact scope and the byte-level verification are in "Provenance" at the end of the archive
timeline.

> **Why the file name carries a `b` suffix**: the primary-result batch has
> `meta.generated_at = 2026-11-12T07:26:25+00:00` (the `score` re-scoring moment; the original retrieval finished at
> `07:18:32`), which **collides** with the already archived `2026-11-12-report.*` (the previous-round baseline). To
> avoid overwriting or rewriting a historical archive, the batch is named `2026-11-12b-report.json` / `.md`, meaning
> **the second batch of the same day**; the two are distinguishable both by `meta.generated_at` and by the binary
> `sha256` prefix.

## Archive timeline

| batch | files (primary-result backend is `nltk` for all three) | overall accuracy | cpp | rust | binary `sha256` prefix | notes |
| --- | --- | --- | --- | --- | --- | --- |
| **2026-11-12b** (**current primary result**) | `2026-11-12b-report.md` / `.json` | **84.0%** (84/100) | **55.0% (11/20)** | 90.0% (18/20) | `b1532bb1b43e3d65` | **+2.0pt** over the previous-round baseline; source of the change = **rank-time enclosing-candidate demotion (cpp only)**, cpp **+2 tasks (45.0% → 55.0%)**, **zero regression in the other four languages** |
| 2026-11-12 (previous-round baseline: rust `mod_item` fix) | `2026-11-12-report.md` / `.json` | 82.0% (82/100) | 45.0% (9/20) | 90.0% (18/20) | `38edff1bb1b1d1cd` | **+8.0pt** over the pre-fix baseline; **8 tasks flipped to pass, 0 regressions**, and all 8 were **rust** |
| 2026-11-11 (pre-fix baseline, historical comparison) | `2026-11-11-report.md` / `.json` | 74.0% (74/100) | 45.0% (9/20) | 50.0% (10/20) | `5972636dc15431ee` | pre-fix baseline, used only to size the improvement |

All three batches use exactly the same retrieval protocol: the same 100-task subset (5 languages × 2 repos × 10
needles), the same dataset (`sha256` prefix `bd3f7cab47283cde`, release `2024-06-23`), the same `threshold = 0.8`,
`hybrid: 100` (`name_fallback: 0`, `unknown_empty: 0`, i.e. no degradation and no empty results).

- **74.0% → 82.0%**: the difference comes **entirely from rust** (50.0% → 90.0%, +40.0pt); the python / cpp / java /
  typescript components are identical item by item.
- **82.0% → 84.0%**: the difference comes **entirely from cpp** (45.0% → 55.0%, +2 tasks); the
  **80 non-cpp tasks have bit-for-bit identical `best_similarity`** (not "the same aggregate score" -- the
  floating-point values are exactly equal), so "zero regression in the other four languages" is a hard fact that can
  be verified task by task.

> **BLEU backend note (the primary-result batch is `nltk`)**: the batch was
> **first produced with the `builtin` backend** (nltk was not installed at run time, `meta.bleu_backend = builtin`,
> the built-in replica), and nltk was installed afterwards to re-score the **same `raw/` retrieval results** with
> `repoqa_eval score` (`meta.command = score`, `meta.rescore_only = true`, no retrieval re-run). The re-scored result
> is **bit-for-bit identical** to `builtin`: overall **84.0% (84/100)**, the same per-language split
> **18 / 11 / 17 / 20 / 18**, **0 flips in per-task `passed`, maximum absolute `best_similarity` error 0.0**. The two
> backends are therefore **equivalent and interchangeable on this batch**, and the archived `2026-11-12b-report.json`
> / `.md` is exactly the `nltk` re-scored version (the `builtin` version survives only as the cross-check intermediate
> at `/tmp/repoqa-eval/report.builtin.{json,md}` and is not committed).

From the pre-fix baseline (74.0%) to the primary result (84.0%), **10 tasks went fail -> pass with 0 regressions**
(obtained by comparing `per_task.passed` between the two batches):

| # | task id | repository | attribution |
| --- | --- | --- | --- |
| 1 | `rust__rust-bakery__nom__n01` | rust-bakery/nom | rust `mod_item` recursion (the +8.0pt round) |
| 2 | `rust__rust-bakery__nom__n03` | rust-bakery/nom | same as above |
| 3 | `rust__rust-bakery__nom__n04` | rust-bakery/nom | same as above |
| 4 | `rust__rust-bakery__nom__n06` | rust-bakery/nom | same as above |
| 5 | `rust__rust-bakery__nom__n08` | rust-bakery/nom | same as above |
| 6 | `rust__helix-editor__helix__n00` | helix-editor/helix | same as above |
| 7 | `rust__helix-editor__helix__n06` | helix-editor/helix | same as above |
| 8 | `rust__helix-editor__helix__n09` | helix-editor/helix | same as above |
| 9 | `cpp__apache__logging-log4cxx__n00` | apache/logging-log4cxx | cpp rank-time enclosing demotion (`best_similarity` 0.246 → **1.0**) |
| 10 | `cpp__skypjack__uvw__n03` | skypjack/uvw | cpp rank-time enclosing demotion (`best_similarity` 0.217 → **1.0**) |

> Before the fix, tasks 9 and 10 both had a `best_target` **with the same name** as the needle (the correct symbol had
> already been retrieved at Top-1); the returned `code_block` was a strict superset of it -- the enclosing container
> (class/struct) -- which diluted BLEU to around 0.2. After demoting containers the member block reaches rank1 and the
> similarity is a perfect score.

> **What the `.builtin.*` suffix means**: it marks a copy re-scored with the **built-in BLEU replica backend**
> (`backend=builtin`, the fallback used when nltk is missing) on the same retrieval results, for cross-checking
> against the primary result. **All three batches use `nltk` as the primary result**, i.e. the `.md` / `.json` files
> without a suffix; only the 2026-11-11 baseline keeps a `.builtin.*` copy in the repository.
> **The `builtin` version of the 2026-11-12b batch is not committed** and lives only at
> `/tmp/repoqa-eval/report.builtin.{json,md}`; against the committed `nltk` version it shows
> **0 flips in per-task `passed` and a maximum absolute `best_similarity` error of 0.0** (see the "BLEU backend note"
> above). The 2026-11-11 batch was cross-checked the same way over all 100 tasks: no per-task `best_similarity`
> difference and no `passed` flips, so the two score sets are identical.
>
> **Provenance**: all four `.md` files are produced by `render_markdown()` in `repoqa_eval/report.py`; no number was
> edited by hand, and `md == render_markdown(json)` was verified byte for byte on every file (all `True`). Measured
> values of the archived files:
>
> | file | bytes | md5 |
> | --- | --- | --- |
> | `2026-11-11-report.md` | 4173 | `8f7aeb70664f99afd930f10a9879ce21` |
> | `2026-11-11-report.builtin.md` | 4490 | `3beb1694991f4038a429fed7dfe6d60b` |
> | `2026-11-12-report.md` | 4251 | `b8a222f721c651f44dc02366bed6926e` |
> | `2026-11-12b-report.md` | 4128 | `20c020aa8be1cd717c0b1133c0057982` |
>
> Only two `.json` files differ from their original copies, and both were edited at **prose level only**:
> `meta.embedding_note` in `2026-11-12-report.json`, and `meta.embedding_note` plus `warnings[0]` in
> `2026-11-11-report.builtin.json`; every other key and value is untouched, and the other two `.json` files are
> verbatim copies. All batches share the same section structure, table headers and wording because they come from the
> same renderer.

## 1. Key takeaways

**Overall accuracy: 84.0% (84/100 tasks passed, threshold = 0.8)**

| language | passed / total | accuracy |
| --- | --- | --- |
| python | 18/20 | 90.0% |
| cpp | 11/20 | 55.0% |
| java | 17/20 | 85.0% |
| typescript | 20/20 | 100.0% |
| rust | 18/20 | 90.0% |
| **all** | **84/100** | **84.0%** |

Per-repo detail is in Section 3 of `2026-11-12b-report.md`: `psf/black` 10/10, `xenova/transformers.js` 10/10,
`expressjs/express` 10/10, `helix-editor/helix` 10/10 (all 100.0%); `square/retrofit` 9/10 (90.0%);
`python-poetry/poetry` 8/10, `google/gson` 8/10, `rust-bakery/nom` 8/10 (all 80.0%); `skypjack/uvw` 6/10 (60.0%);
`apache/logging-log4cxx` 5/10 (50.0%). The components sum to the table above (18+11+17+20+18 = 84).

### 1.1 Where the two rounds of improvement came from

| round | change | location | effect |
| --- | --- | --- | --- |
| first (74.0% → 82.0%) | `parse_block` gained a `mod_item` branch; `impl_item`'s `full_range` narrowed to `declaration_range` | `rust-core/src/codegraph/treesitter/parsers/rust.rs` (commit `9e74d94`) | rust 50.0% → 90.0% (+40.0pt), overall **+8.0pt**, 8 tasks flipped, 0 regressions |
| second (82.0% → 84.0%) | **rank-time** enclosing-candidate demotion `demote_enclosing_candidates`, language whitelist `cpp` only | `rust-core/src/services/hybrid_search.rs` (`ENCLOSING_DEMOTE_LANGUAGES = ["cpp"]`, commit `571fe6c`) | cpp 45.0% → 55.0% (+2 tasks), overall **+2.0pt**, zero regression in the other four languages |

The two fixes of the first round (see Section 5 of `../README.md` and the comments in `hybrid_search.rs`):

1. **`mod_item` branch**: `mod_item` used to fall into `_ =>` and go through `parse_usages`, whose `_ => {}` fallback
   **does not recurse** -> the whole `#[cfg(test)] mod tests { ... }` subtree produced nothing. Many of RepoQA's rust
   needles sit inside `mod tests`, which is the main cause of the rust recall gap.
2. **Narrowing `impl_item`'s `full_range`**: impl container chunks no longer cover the source range of their member methods.

The second round takes a different route from the first --
**it leaves chunking (the index side) alone and only changes ranking**: no chunk's `content` is modified; instead,
after hybrid fusion the candidate list is scanned for parent/child pairs where "candidate i's `[line_start, line_end]`
strictly encloses candidate j", and anything enclosed >=1 time is multiplied by `0.5`, enclosed >=2 times by another
`0.5` (floor x0.25), which pushes "giant container blocks" off rank1 and lets member blocks take over. Why the gate is
cpp-only is shown by the measured table in Section 2.

## 2. Rejected approaches (measured, so nobody has to redo them)

The first three rows are approaches that were **measured and rejected / rolled back**; the fourth row is the approach
**currently in use**. All three rejections came from **clearing the index and re-running all 100 tasks**, differing
from the current primary result only in the change under test. They are recorded here so that nobody repeats them.

| approach | what was done | measured | verdict |
| --- | --- | --- | --- |
| chunking-layer content degradation | parent container chunks' `content` changed from `full_range` to the `declaration_range` signature skeleton (both chunkers updated in step) | **78.0%** (cpp up to 55%, but typescript 100%→70%, rust 90%→85%) | **rejected**. Degraded containers become "a very short document plus a perfect symbol-name hit", so BM25 length normalisation pushes them back to rank1 while the body holds nothing but the signature -> BLEU near 0 |
| rank-time enclosing demotion for every language | `demote_enclosing_candidates` applied to all languages | **81.0%** (cpp 55%, java 90%, but typescript 85%, rust 85%) | **rejected**. typescript needles are often the class itself, and in rust the correct block gets misjudged by the 60-line window span; both regress |
| BM25 storing the real `line_end` | STORED `line_start` / `line_end` added to the tantivy schema and backfilled by boundary | **48.0%** (rust entirely `unknown_empty`, all 10 express tasks empty) | **rolled back**. Slicing by `line_end` produced empty bodies; already `git revert`ed |
| **rank-time enclosing demotion (cpp only)** | same as above, but the language gate is `cpp` | **84.0% (cpp 55%, zero regression elsewhere)** | **adopted** (current primary result) |

> Common premise of all four attempts:
> **any "less content" change in the chunking layer (chunker) moves BM25 document length and ranking at the same time**,
> so an improvement in one language usually buys a regression in another; a rank-time change only touches ranks and
> can be gated per language, which is exactly why "cpp only" delivers a net +2 tasks with zero regression.
>
> Note that cpp sits at **55.0%** in both intermediate results, `78.0%` and `81.0%` (the same as in the primary
> result): there are several implementation paths that lift cpp from 45% to 55%, but only rank-time demotion
> restricted to cpp **does not hurt the other languages**.

## 3. Evaluation conditions

> The table below describes the **2026-11-12b primary result**. The two earlier batches use the same protocol with
> only three values different: the binary under test (previous round `38edff1bb1b1d1cd`, pre-fix `5972636dc15431ee`),
> the `--concurrency` of `run` (`4` for the pre-fix baseline, `1` for the others), and where the scoring backend came
> from (the primary-result batch was produced with `builtin` and re-scored to `nltk` by `score`, the two earlier
> batches were produced with `nltk` directly); all three end up with backend `nltk`.

| item | value |
| --- | --- |
| data subset protocol | 5 languages × 2 repos × 10 needles = **100 tasks** (`python/cpp/java/typescript/rust`, **no go**; see Section 5 of `../README.md`) |
| dataset | official release `2024-06-23`, `size_bytes=71510991`, `sha256` prefix `bd3f7cab47283cde` |
| decision threshold | `threshold = 0.8` (BLEU-4 + Chen & Cherry `method4`, candidate set = that repo's 10 needles) |
| retrieval mode | `hybrid: 100` (100.0% genuine semantic retrieval: LanceDB + BM25, results carry `code_block`) |
| empty results | `name_fallback: 0`, `unknown_empty: 0`, i.e. **0 empty results, 0 degradation** |
| `search_limit` | 5 (the Top-5 of every task is persisted, scoring only uses Top-1) |
| tree-sitter | `false` (optional dependency, not installed; only affects the "first function" extraction step from the model output) |
| binary under test | `rust-core/target/release/codeseek`, `sha256` prefix `b1532bb1b43e3d65`, `size_bytes=60600320`, mtime `2026-11-12T07:05:19.955112+00:00` |
| codeseek version | `codeseek 0.1.31` (from `codeseek --version`) |
| embedding config | `embedding configured (model=Qwen/Qwen3-Embedding-4B, api_token length 51)` |
| BLEU backend | `nltk` (the same one the official `repoqa.compute_score` uses); the batch was produced with `builtin` first and re-scored by `score` with bit-identical conclusions, see the "BLEU backend note" above |
| run scale | retrieval: `generated_at=2026-11-12T07:18:32+00:00`, `elapsed_s=198.2`, `concurrency=1`, `init_failures=[]`; re-score: `command=score`, `rescore_only=true`, `generated_at=2026-11-12T07:26:25+00:00` (no retrieval re-run, only the BLEU backend changes on the same `raw/`) |

## 4. Three key insights

### 4.1 The passing group's function boundaries come from the same source as the needle definition

Of the 84 passing tasks, **83** have a `best_similarity` of exactly `1.0` (a perfect BLEU-4 score, meaning the
returned `code_block` is identical to the ground truth, not merely "similar"). The only non-perfect passing task,
identical across all three batches, is `python__python-poetry__poetry__n01` (`best_similarity=0.946`): its
`code_block` carries one extra `@classmethod` decorator line compared with the GT and still passes with BLEU >= 0.8.

> Implication: codeseek's chunk boundaries on the index side line up closely with RepoQA's needle slicing, so the
> passes are not scraping over the line on BLEU tolerance.

### 4.2 cpp 45.0% → 55.0% was claimed by rank-time enclosing demotion; the bottleneck then moved from "block too big" to "recall / ranking"

Both flipped tasks (`cpp__apache__logging-log4cxx__n00`, needle `ConfiguratorWatchdog`, and `cpp__skypjack__uvw__n03`,
needle `write_req`) had a `best_target` **with the same name** as the needle in the previous-round baseline, but a
`best_similarity` of only **0.246 / 0.217** -- the symbol was already recalled, the block was the parent container.
Demotion pushes the parent container behind rank2, the member block reaches rank1 and the similarity becomes **1.0**
outright. This matches the previous round's diagnosis ("the container chunk, not the member chunk, was taking rank1")
exactly.

But **the nature of the remaining 9 cpp failures has changed**:

| nature | tasks | task ids | notes |
| --- | --- | --- | --- |
| `below_threshold` (same name, block mismatch) | **2** | `logging-log4cxx__n04` (`catch`, 0.024), `uvw__n04` (`recv`, 0.035) | still a "block boundary" problem, but the similarity is very low (0.02~0.04), so the returned block is almost completely different from the needle -- not "slightly bigger" |
| `wrong_target` (a different symbol came back) | **7** | `logging-log4cxx__n05` `n06` `n07` `n08`, `uvw__n02` `n06` `n07` | the returned `best_target` has a different name from the needle (e.g. `PropertiesPatternConverterPrivate` → `ConfiguratorWatchdog`), i.e. **not recalled** |

> Conclusion: the cpp "chunk granularity = P0" problem **was closed for two tasks by rank-time demotion**; 7 of the
> remaining 9 are **recall failures** (the needle never enters a usable block in the Top-5) and are no longer a
> chunk-granularity problem. That is also why continuing to "degrade content" in the chunking layer drags
> typescript/rust down without buying anything (Section 2).

### 4.3 rust holds at 90.0%; the remaining 2 tasks are about boundaries and same-name ranking

| task | needle | best_target | sim | reason | nature |
| --- | --- | --- | --- | --- | --- |
| `rust__rust-bakery__nom__n07` | `be_i8_tests` | `be_i8_tests` | 0.721 | `below_threshold` | **chunk-boundary difference** (the symbol is recalled, the block is longer or shorter than the needle) |
| `rust__rust-bakery__nom__n09` | `length_count` | `parse` | 0.017 | `wrong_target` | still recall / same-name interference |

`nom__n07` is the "closest to passing" task among all 16 failures (0.721 against a threshold of 0.8), and in the
pre-fix baseline it was `wrong_target` (it returned `many1_count`), so this round moved it from "not recalled" to
"boundary mismatch".

## 5. Known limitations and next steps

- **For cpp the next step is recall, not chunking**: 7 of the 9 remaining cpp failures are `wrong_target` (Section
  4.2). Further chunker changes have already been measured and rejected (78.0% / 81.0% in Section 2); the directions
  worth exploring are cpp symbol coverage (macro expansion, template specialisations, static functions inside
  anonymous namespaces) and the ranking of same-named symbols.
- **Rank-time demotion depends on the accuracy of `line_start` / `line_end`**: on the dense-only path the span comes
  from real line numbers in the index (trustworthy); on the path that goes through `hydrate_code_blocks` backfill, a
  missing `line_end` degrades to a fixed window of "`line_start` + 59", so
  **containers longer than 60 lines may be identified incompletely**. Whether this affects the remaining cpp failures
  is **unverified**; it is recorded here as a known precision loss only.
- **Demotion pushes containers down, it never lifts them**: the approach assumes "a giant container is almost never
  the needle". That holds for cpp (+2 tasks), but it backfires for typescript (needles are often the class itself) and
  rust (`impl`/`mod` are often the answer) -- see the 81.0% row in Section 2 -- so the gate must not be widened.
- **`code_block` granularity directly moves BLEU**: the code block returned by hybrid results is cut on the index side
  and may include decorators/comments or be truncated; `threshold=0.8` is quite sensitive to that (the BLEU of
  `be_i8_tests` is less than 0.1 away from the threshold).
- **The non-cpp components have reached a plateau**: python 90.0%, java 85.0%, typescript 100.0%, rust 90.0%,
  identical across all three batches; the 5 python/java failures are mostly `wrong_target` (ranking among same-named
  symbols), not a chunking problem.
- **No granularity-tolerant metric was measured**: this archive reports the official protocol only (strictly
  `best_target == GT and BLEU >= 0.8`). A lenient protocol of "the GT falls anywhere inside the returned block" would
  raise the cpp score substantially, but that number is not comparable with the official leaderboard, so it stays out
  of the conclusions.
- Other protocol limitations (candidate set of 10 needles only, `git init` snapshot instead of real history,
  degradation to name matching without a token, etc.) are in Section 5 of the parent [`../README.md`](../README.md).

## 6. Reproduction

```bash
cd evals/repoqa
pip install -r requirements.txt                                    # install nltk to use the official BLEU
python -m repoqa_eval prepare --workdir /tmp/repoqa-eval           # download data + select the 100-task subset + git init
python -m repoqa_eval run --workdir /tmp/repoqa-eval --limit 5 --threshold 0.8 --concurrency 1
python -m repoqa_eval score --workdir /tmp/repoqa-eval             # re-score with another threshold, no retrieval re-run
```

> The only run parameter that differs between the three batches is `--concurrency` (`4` for the pre-fix baseline, `1`
> for the other two); after the concurrency fix (`c56a41f`) the two settings are equivalent. The primary-result batch
> was first produced in an environment without nltk with `meta.bleu_backend=builtin`; nltk was installed afterwards
> and `score` was run over the **same `raw/`** (`meta.command=score`, `rescore_only=true`, no retrieval re-run), so
> the archived `2026-11-12b-report.*` is that `nltk` re-scored version: overall, per-language and per-task `passed`
> are identical to the `builtin` version (maximum absolute `best_similarity` error 0.0).
>
> The binary under test for the primary-result batch is `rust-core/target/release/codeseek` (`sha256` prefix
> `b1532bb1b43e3d65`), corresponding to commit `571fe6c`
> (`fix(hybrid-search): revert line_end schema, gate demotion to cpp`).

For the evaluation protocol, the scoring implementation details and known limitations see the parent
[`../README.md`](../README.md).
