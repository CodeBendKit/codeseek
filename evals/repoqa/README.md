# RepoQA × codeseek end-to-end evaluation suite

Measures codeseek's **long-context code retrieval** on the [RepoQA](https://github.com/evalplus/repoqa_release) benchmark
(arXiv:2406.06025, ICML 2024). The whole suite uses only the Python standard library (plus optional nltk), and every
artifact is written under `--workdir` (default `/tmp/repoqa-eval`), so the **repository is never polluted**.

> The scoring logic is a verbatim re-implementation of the official `repoqa/metric.py` and `repoqa/compute_score.py`:
> BLEU-4 (whitespace tokenization) + Chen & Cherry `method4` smoothing + that repo's 10 needles as the candidate set +
> `threshold=0.8`. **No LLM takes part in scoring.**

---

## 1. Pipeline

```
                          ┌──────────────────────────────────────────────────────────────────────┐
  GitHub release ────────►│data.download_dataset   (12.5MB gz -> 71.5MB)                         │
                          │  cache reuse; atomic rename via .part                                │
                          │  validate_dataset  (6 languages x 10 repos)                          │
                          │  offline: REPOQA_DATA_OVERRIDE_PATH                                  │
                          └───────────────────────────────────┴──────────────────────────────────┘
                                                              ▼
                          ┌──────────────────────────────────────────────────────────────────────┐
  prepare ───────────────►│data.select_subset   5 languages x 2 repos x 10 needles               │
                          │  -> 100 tasks (deterministic, not random)                            │
                          │data.materialize_repo -> repos/<lang>/<slug>/                         │
                          │  + git init  (hard prerequisite for codeseek init)                   │
                          │data.write_tasks -> tasks.jsonl + meta.json                           │
                          └───────────────────────────────────┴──────────────────────────────────┘
                                                              ▼
                          ┌──────────────────────────────────────────────────────────────────────┐
  run ───────────────────►│runner.run                                                            │
                          │  1. serial:   codeseek init  (once per repo)                         │
                          │  2. parallel: codeseek search "<description>"                        │
                          │       --json --limit K                                               │
                          │     adapter.parse_json_tolerant -> results                           │
                          │     adapter.build_outputs -> ```lang fenced code block               │
                          │     write raw/<task_id>.json  (resumable runs)                       │
                          │  3. scorer.score_tasks  (BLEU + 10 needles)                          │
                          │  4. report.build_report / write_reports                              │
                          └───────────────────────────────────┴──────────────────────────────────┘
                                                              ▼

               report.json (machine-readable) + report.md (human-readable, English)
                                                              ▲
  score ─────────────────►│re-score raw/*.json only  (new threshold, no re-run)                  │
```

## 2. Prerequisites

| dependency | notes |
| --- | --- |
| Python >= 3.10 | standard library only; type annotations use the `X \| None` syntax |
| codeseek binary | `cargo build --release --manifest-path rust-core/Cargo.toml` -> `rust-core/target/release/codeseek` (or `--codeseek-bin` / `CODESEEK_BIN` / `--auto-build`) |
| nltk (**optional but strongly recommended**) | `pip install -r requirements.txt`. When missing, the built-in method4 replica is used, `meta.bleu_backend` reports `builtin`, and the report carries a warning |
| embedding token (**decides whether scores are comparable**) | semantic retrieval runs in hybrid mode only when `embedding.api_token` in `~/.codeseek/config.json` is non-empty |
| git | `materialize_repo` runs `git init` once; codeseek `init` fails with `No project found` outside a git directory |
| tree-sitter (optional) | the official `sanitize_output` uses it to pick "the first function definition"; when it is missing this suite uses the whole code block instead of failing |

## 3. The three commands

```bash
cd evals/repoqa
pip install -r requirements.txt          # optional: installs nltk

# 1. prepare: download the data + select the subset + materialize repos + git init + tasks.jsonl
python -m repoqa_eval prepare --workdir /tmp/repoqa-eval
#    python and rust only, one repo each (10 tasks):
python -m repoqa_eval prepare --workdir /tmp/repoqa-eval --languages python,rust --repos-per-language 1

# 2. run: build check first, then serial init, then concurrent search
python -m repoqa_eval run --workdir /tmp/repoqa-eval --limit 5 --concurrency 4
#    smoke test: first 3 tasks only; semantic retrieval is mandatory, otherwise exit code 2
python -m repoqa_eval run --workdir /tmp/repoqa-eval --limit-questions 3 --require-embedding

# 3. re-score with another threshold (no search re-run, only raw/ is read)
python -m repoqa_eval score --workdir /tmp/repoqa-eval --threshold 0.7

# 4. offline self-test (no network, no codeseek binary, no nltk)
python -m repoqa_eval selftest
```

Common environment variables:

| variable | effect |
| --- | --- |
| `REPOQA_DATA_OVERRIDE_PATH` | points at an existing local `repoqa-2024-06-23.json` (or `.json.gz`) and skips the download |
| `CODESEEK_BIN` | path of the codeseek binary |
| `CODESEEK_CONFIG_PATH` | overrides the `~/.codeseek/config.json` path (handy for exercising the warning logic with a fake config) |
| `https_proxy` / `http_proxy` | route the download through a proxy (or `prepare --proxy http://127.0.0.1:7897`) |

## 4. Scoring protocol (rule by rule)

1. **Tokenization**: `re.split(r"\s+", text.strip())` -- split on whitespace, not at AST/token level.
   Note that the official implementation **keeps empty strings** (empty input yields `[""]`); this suite matches it.
2. **Similarity**: `nltk.translate.bleu_score.sentence_bleu([ref], cand, smoothing_function=SmoothingFunction().method4)`
   with the default weights `(0.25, 0.25, 0.25, 0.25)` (BLEU-4).
   Key points of the built-in replica (bit-for-bit identical to nltk 3.10.4, cross-checked on 308 samples, max error `0.0`):
   - the denominator of modified precision is `max(1, number of n-grams in the hypothesis)`;
   - **short-circuit first**: if not a single unigram matches, the score is 0 outright (smoothing cannot rescue it);
   - method4: `incvnt` starts at 1 and only increments on a substitution; the substituted value is
     `(1 / (2**incvnt * k / log(hyp_len))) / denominator` with `k=5`, and it only applies when
     `p_i.numerator == 0 and hyp_len > 1`;
   - finally `bp * exp(fsum(w_i * log(p_i) for p_i > 0))`.
3. **sanitize**: the official regex `^```(?:\w+)?\s*\n(.*?)(?=^```)```` (`re.DOTALL|re.MULTILINE`) takes the first
   fenced code block; if nothing matches, the whole output is used; if tree-sitter is installed, the first function
   definition inside the block is taken instead.
4. **Candidate set**: **only the 10 needles of that repo** (not every function in the repository).
   A candidate slice is `"\n".join(content[path].split("\n")[start_line:end_line])`, with 0-based line numbers inside
   the file and an exclusive `end`.
5. **Decision**: `best_target == ground_truth_name and best_similarity >= threshold`, with `threshold` defaulting to
   `0.8`; `best_target` is the needle with the **highest** BLEU among the 10 (strict `>` comparison, ties go to the
   first occurrence).
6. **Top-1 output only**: the official code reads `output[0]` and nothing else; this suite assembles codeseek's Top-1
   retrieval result into a fenced code block as `output[0]`.
7. **No LLM involved**: no generative model scores anything anywhere in the pipeline.

## 5. Known limitations and risks

- **Scores are not comparable without a token**: when `embedding.api_token` in `~/.codeseek/config.json` is empty,
  codeseek's `search` degrades to `graph.find_functions_by_name(query)`, i.e. **function-name substring matching**
  (the JSON elements carry no `code_block`). The score then measures "name matching" rather than semantic retrieval;
  `report.md` opens with a blockquote warning and `meta.mode_counts.name_fallback` reports the task count.
- **The candidate set holds only 10 needles**, far more permissive than "search every function in the repository";
  official leaderboard numbers cannot be swapped with a self-built whole-repository evaluation.
- **The official release is really 6 languages / 600 tasks** (its README says 5 languages / 500 tasks). This suite
  defaults to the 5 languages `python/cpp/java/typescript/rust` (no `go`); adjust with `--languages`.
- **`code_block` granularity may not match the needle**: the code block returned by hybrid results is cut on the index
  side and may include decorators/comments or be truncated, which directly moves BLEU (this is the main noise source
  behind the BLEU >= 0.8 pass bar).
- **`position_ratio` is really 0.05~0.95**: the official code computes `(i + 0.5) / 10`; this suite copies that and
  only uses the value for display in the report.
- **Differences from the official `repoqa.compute_score`**: this suite does not require tree-sitter; and "when Top-1
  yields no extractable source, the next result that does provides the output" (the official code has no such notion,
  it only ever has one model output). Both differences are noted in `report.md` (tree-sitter status lives in
  `meta.treesitter_available`).
- **After `git init` the index is a snapshot, not the original repository history**: RepoQA's `content` holds only the
  referenced source files, so fewer symbols are covered than in the real repository and retrieval difficulty differs
  systematically from the official protocol.
- **`codeseek init` stdout is polluted by a progress bar**: `adapter.init_repo` does not parse the output at all, it
  only checks the exit code; the `search` JSON goes through a three-stage tolerant parser (whole document ->
  `[..]`/`{..}` extraction -> line-by-line NDJSON).
- **[RESOLVED] cpp chunk-granularity problem (the old P0)**: in the previous-round baseline **7** of the 11 failing
  `cpp` tasks had a `best_target` **with the same name** as the needle (the correct symbol had already been retrieved
  at Top-1); they failed only because the returned `code_block` was a **strict superset** of the needle -- the
  enclosing container (class/struct) -- so BLEU stayed below 0.8 (e.g. `ThrowableInformationPatternConverterPrivate`,
  sim=0.392). **The container chunk, not the member chunk, was taking rank1.**
  The problem **was fixed by rank-time enclosing-candidate demotion (cpp only)**: `demote_enclosing_candidates` in
  `hybrid_search.rs` together with `ENCLOSING_DEMOTE_LANGUAGES = ["cpp"]` pushes container chunks down the ranking so
  the member block wins; `cpp` went **45.0% → 55.0%** (+2 tasks, both `best_similarity` 0.246 / 0.217 → **1.0**),
  overall **82.0% → 84.0%**, with zero regression in the other four languages. **The content-degradation variant at the
  chunking layer was measured and rejected** (container `content` reduced to the `declaration_range` signature
  skeleton: **78.0%** measured -- containers became "very short documents plus a perfect symbol-name hit", BM25 length
  normalisation pushed them straight back to rank1, typescript 100%→70%, rust 90%→85%). See Section 2 of
  [`results/README.md`](results/README.md).
- **The remaining cpp bottleneck has turned into recall**: of the 9 failing cpp tasks only 2 share the needle's name
  (`below_threshold`), the other 7 are `wrong_target` (a different symbol came back, e.g.
  `PropertiesPatternConverterPrivate` → `ConfiguratorWatchdog`), which is no longer a chunk-granularity problem. The
  next things to look at are cpp symbol coverage (macros, template specialisations, static functions inside anonymous
  namespaces) and the ranking of same-named symbols.
- **The top-1 failure is a boundary difference, not a recall failure**: `rust__rust-bakery__nom__n07` (`be_i8_tests`)
  has a `best_target` **with the same name** as the needle but `best_similarity=0.721 < 0.8`, i.e. a **chunk-boundary
  difference**; in the pre-fix baseline this task was `wrong_target` (it returned `many1_count`), so it has moved from
  "not recalled" to "boundary mismatch".

## 6. Layout and artifacts

```
evals/repoqa/
├── README.md
├── requirements.txt
├── results/                # archived measured results that ran end to end (see Section 8)
│   ├── README.md
│   ├── 2026-11-12b-report.md          # primary result, backend=nltk, 84.0%
│   ├── 2026-11-12b-report.json
│   ├── 2026-11-12-report.md           # previous-round baseline, backend=nltk, 82.0%
│   ├── 2026-11-12-report.json
│   ├── 2026-11-11-report.md           # pre-fix baseline, backend=nltk, 74.0%
│   ├── 2026-11-11-report.json
│   ├── 2026-11-11-report.builtin.md   # backend=builtin (cross-check baseline)
│   └── 2026-11-11-report.builtin.json
└── repoqa_eval/
    ├── __init__.py
    ├── data.py      # download / cache / subset sampling / repo snapshots / Task / needle slicing
    ├── bleu.py      # BLEU (nltk preferred + built-in method4 replica)
    ├── scorer.py    # sanitize + 10-needle candidate set + threshold
    ├── adapter.py   # binary detection / init / search / tolerant JSON parsing / output assembly
    ├── runner.py    # serial init + concurrent search + resumable runs + scoring orchestration
    ├── report.py    # report.json / report.md
    ├── cli.py       # prepare / run / score / selftest
    └── __main__.py  # python -m repoqa_eval
```

`<workdir>` layout:

```
/tmp/repoqa-eval/
├── repoqa-2024-06-23.json.gz     # original gz (12.5MB)
├── repoqa-2024-06-23.json        # decompressed (71.5MB)
├── meta.json                     # dataset_path / languages / repos_per_language
├── tasks.jsonl                   # one line per task (id/language/repo/needle/description/position_ratio...)
├── repos/<language>/<owner>__<name>/   # repo snapshot + .git + .repoqa_manifest.json
├── raw/<task_id>.json            # per-task retrieval result (outputs/mode/elapsed/error)
├── report.json
└── report.md
```

## 7. Resumable runs and troubleshooting

- **Resuming**: if `raw/<task_id>.json` already exists the search is skipped; if every task of a repo has a raw file,
  even `codeseek init` is skipped (it is the most expensive step). `--force` re-runs everything.
- **A single repo fails to init**: all tasks under that repo are recorded as failed (`error="init_failed: ..."` in the raw
  file) without aborting the run; `report.md` lists the failing repositories.
- **A single search times out or exits non-zero**: recorded as failed and the run continues; `raw.error` keeps the first
  500 characters of stdout/stderr.
- **Binary not found**: the error message prints the exact
  `cargo build --release --manifest-path rust-core/Cargo.toml` command; add `--auto-build` to let the script build it
  (only executed when you explicitly ask for it).
- **Many `unknown_empty` in the report**: search returned no parsable JSON (index not built / binary mismatch).
  Reproduce by hand first with `cd <repo> && codeseek init && codeseek search "..." --json` to locate the failure.

## 8. Measured results

**Current primary result: the 2026-11-12 (b) report, overall 84.0% (84/100)** (100-task subset, 5 languages × 2 repos
× 10 needles, `threshold=0.8`, `hybrid` retrieval on 100% of the tasks, 0 empty results; binary under test `sha256`
prefix `b1532bb1b43e3d65`):

| language | passed / total | accuracy |
| --- | --- | --- |
| python | 18/20 | 90.0% |
| cpp | 11/20 | 55.0% |
| java | 17/20 | 85.0% |
| typescript | 20/20 | 100.0% |
| rust | 18/20 | 90.0% |
| **all** | **84/100** | **84.0%** |

- Report and per-task detail: [`results/README.md`](results/README.md) (`results/2026-11-12b-report.md` / `.json` is
  the primary result)
- Improvement path (two rounds, both inside `rust-core/`):
  1. **74.0% → 82.0% (rust)**: in `rust-core/src/codegraph/treesitter/parsers/rust.rs`, `parse_block` gained a
     `mod_item` branch and `impl_item`'s `full_range` was narrowed to `declaration_range`; rust 50.0% → 90.0%
     (+40.0pt, 8 tasks flipped, 0 regressions).
  2. **82.0% → 84.0% (cpp)**: `demote_enclosing_candidates` in `services/hybrid_search.rs` (**rank-time**
     enclosing-candidate demotion, `ENCLOSING_DEMOTE_LANGUAGES = ["cpp"]`); cpp 45.0% → 55.0% (+2 tasks) and zero
     regression in the other four languages.
- Key insights are in Section 4 of [`results/README.md`](results/README.md); the **three measured-and-rejected
  approaches** (chunking-layer content degradation 78.0%, demotion for all languages 81.0%, storing the real
  `line_end` in BM25 48.0%) are in its Section 2.

**Historical comparison (previous-round baseline)**: the first 2026-11-12 batch, overall 82.0% (82/100), cpp 45.0%
(9/20), binary `sha256` prefix `38edff1bb1b1d1cd` (`results/2026-11-12-report.md` / `.json`).

**Historical comparison (pre-fix baseline)**: the first end-to-end evaluation, on 2026-11-11, overall 74.0% (74/100),
rust 50.0% (10/20), binary `sha256` prefix `5972636dc15431ee`; that batch is kept as a baseline only
(`results/2026-11-11-report.md` / `.json`, plus the builtin-BLEU cross-check baseline `*.builtin.*`).
