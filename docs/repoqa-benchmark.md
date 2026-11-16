# RepoQA 评测基准调研

> 一句话简介：RepoQA 是首个面向长上下文代码理解（long-context code understanding）的评测基准，首个任务 SNF（Searching Needle Function）要求模型在 16K token 的仓库代码上下文中，根据一段不含函数名的自然语言描述检索出目标"针函数"。

## 目录

- [1. 基本信息](#1-基本信息)
- [2. 任务形式（SNF）](#2-任务形式snf)
- [3. 数据构成](#3-数据构成)
- [4. 评测指标与判定规则](#4-评测指标与判定规则)
- [5. 使用方式（CLI）](#5-使用方式cli)
- [6. 官方榜单（16K 上下文，thresh = 0.8）](#6-官方榜单16k-上下文thresh--08)
- [7. 论文主要结论](#7-论文主要结论)
- [8. 常见误传与澄清](#8-常见误传与澄清)
- [9. 维护状态](#9-维护状态)
- [10. 参考链接](#10-参考链接)
- [11. 信息核实说明（含常见误传核实与修订记录）](#11-信息核实说明含常见误传核实与修订记录)
- [12. codeseek 自身实测（本地 100 题子集，非官方榜）](#12-codeseek-自身实测本地-100-题子集非官方榜)

## 1. 基本信息

| 项 | 内容 |
|----|------|
| 全称 / 简称 | RepoQA（"Repository Question Answering" 只是社区按字面展开的说法，论文与仓库均未给出正式全称，**未核实**） / RepoQA |
| 提出机构 | UIUC（伊利诺伊大学厄巴纳-香槟分校），Jiawei Liu 等 8 人，Lingming Zhang 为末位作者 |
| 论文 | arXiv:2406.06025，v1 提交于 2024-06-10，学科分类 cs.SE |
| 发表 | ICML 2024 LCFM Workshop（Long-Context Foundation Models Workshop） |
| 代码仓库 | <https://github.com/evalplus/repoqa> ，许可证 Apache-2.0，属 EvalPlus 系列项目 |
| 首个任务 | SNF（Searching Needle Function） |

RepoQA 的定位是把"长上下文能力"从自然语言长文阅读迁移到代码检索场景：模型不再被要求写代码，而是被要求在一个真实仓库的完整上下文中**定位**一段功能已知、但名字未知的代码。

## 2. 任务形式（SNF）

SNF（Searching Needle Function）的输入是一个三元组：

1. **仓库代码上下文**：按文件依赖排序拼接而成的大段仓库代码，长度被固定到 16K token；
2. **目标函数的自然语言描述**：由人工撰写的功能说明，**故意不含函数名等关键词**，避免模型靠字符串匹配"作弊"；
3. **检索指令**：要求模型输出符合该描述的函数实现。

输出即目标函数的代码本身（而不是函数名或文件路径）。

```
[输入 ②] 目标函数的自然语言描述（不含函数名）
"该函数读取项目配置文件，合并默认值后返回最终配置……"

[输入 ①] 仓库代码上下文（按文件依赖排序，固定约 16K tokens）
file_01.ext … file_10.ext（同一仓库的多个源文件顺序拼接）

[输入 ③] 检索指令
"请在上面的仓库代码中找到符合该描述的函数，并输出该函数的完整实现。"

[输出] 目标函数代码（"针函数"，needle function）
```

> 上文代码块是**结构示意**，用于说明三元组的组合方式，并非官方 prompt 的逐字原文。

这正是代码领域的 needle-in-a-haystack：整段仓库上下文是"草堆"，需要被描述定位的那个函数是"针"，而描述刻意避开了函数名这类可直接字符串匹配的锚点。与大海捞针式的检索任务相比，SNF 的难点在于**语义匹配必须精确到函数粒度**——同一仓库里往往存在若干功能相近的函数。

## 3. 数据构成

| 维度 | 内容 |
|------|------|
| 测试数量 | **500**（论文/README 口径，5 语言）｜**600**（官方 release `2024-06-23` 实测，6 语言含 `go`） |
| 语言 | 论文/README 写 Python / C++ / Java / TypeScript / Rust 五种；**官方 release 实测为 6 种，另含 `go`** |
| 仓库 | 每语言 **10** 个 ≥100 stars 的热门仓库，共 **50** 个（论文口径）；release 实测为 **60** 个（含 `go` 的 10 个） |
| 上下文长度 | 固定 **16K tokens**。注意三处口径不一致（原文如此）：论文用 CodeLlama tokenizer；README 的 CLI 默认 `--code-context-size 16384` 但写的是 DeepSeekCoder tokenizer；官方 homepage 写的是 CodeLlama |
| 针函数位置 | 论文表述为 10%、20% … 100% 深度；**实际数据为 0.05、0.15 … 0.95**（代码 `(i + 0.5) / 10`，i=0..9） |

> **口径说明（500 vs 600）**：论文与 README 的「500 = 5 语言 × 10 仓库 × 10 针函数」是**写作时的口径**；后续官方 release 扩充了 `go`，下载 `repoqa-2024-06-23.json.gz` 实测为**6 语言 × 10 仓库 × 10 针函数 = 600 条 needle**（`python` / `cpp` / `java` / `typescript` / `rust` / `go` 各 100 条）。引用数据集规模时应注明采用的是哪一口径。

"深度"指针函数在（按依赖顺序拼接后的）上下文中的**目标**相对位置：0.05 表示针函数被放在靠近上下文开头处，0.95 表示靠近末尾。每个仓库抽取 10 个针函数并覆盖 10 个位置分箱，再由 harness 依次赋予 10 个深度档位——`search_needle_function.py` 对第 i 条 needle 取 `position_ratio = (i + 0.5) / len(repo["needles"])`，并按 `prefix_size = int(code_context_size * position_ratio - ntoken_needle / 2)` 计算前缀长度。因此同一个仓库会被测量 10 次，用于观察"位置越靠后是否越难"。

需要留意：这个 ratio 是**被 padding 出来的目标位置**，不是 needle 在原始仓库文件里的自然字节偏移（实测同一 repo 内不同 needle 的 `global_start_byte / 总字节数` 与 `(i + 0.5) / 10` 并不对应）。论文的 "depths of 10%, 20%, ⋯, 100%" 与实现的 0.05–0.95 相差一档半步（5% 步长、两端各留半档），不要按字面理解成"针函数一定出现在第 10% 处"。

需要留意的是，16K 是**固定窗口**而非可变区间；数据集并不覆盖 2K 或 100K 级别的长度（见 [第 8 节](#8-常见误传与澄清)）。

## 4. 评测指标与判定规则

只有单一指标：**Retrieval Accuracy（0–100%）**。对每个测试而言，判定必须**同时**满足以下条件：

1. **后处理**：用 tree-sitter 从模型输出中提取第一个语法正确的代码块；
2. **(i) 相对排序第一**：该函数与针函数的相似度，在该仓库的**候选集**中最相似。README 原文写的是"与其它经 tree-sitter 系统解析的函数中最接近"，但**实测 `compute_score.py` 的候选集就是该 repo 的 10 条 needle 切片**——按各自 `start_line:end_line` 从 `content` 切出，既不解析仓库、也不枚举全部函数；
3. **(ii) 绝对阈值**：相似度 ≥ 默认阈值 **0.8**；相似度采用带平滑的 BLEU（Chen & Cherry, 2014）。

> **口径差异（实测）**：tree-sitter 只用于从**模型输出**中提取"第一个函数定义"，**不用于构建候选池**。因此判定的真实含义是"模型输出与这 10 条 needle 中哪一条最像，且相似度是否 ≥ 0.8"，比"与全仓库函数竞争排序第一"**宽松得多**：只要在该 repo 的 10 条候选里排第一即可，不存在被仓库中另一个更相似的函数"挤掉"的风险。跨口径比较时（与官方榜单、与自建的全仓库检索评测）分数不能直接互换。

**不使用 LLM-as-judge**：判分完全由语法解析 + BLEU 相似度决定，不存在"让另一个大模型打分"的环节，因此分数是可复现的、确定性的。

README 对判定规则的英文原文：

> "The evaluator passes a test if the searched function is syntactically closest to the ground-truth compared against other functions (systematically parsed by treesitter) and the similarity is greater than a user defined threshold (by default 0.8)."

论文 §3.2 对总分的定义（score computation）：

> "By aggregating the success rate of retrieving needle functions across all tests, we obtain a final accuracy score from 0 to 100 (%) to represent the long-context code retrieval ability of the model."

由于判定要求"在 10 条候选中最相似"与"相似度 ≥ 0.8"同时成立，实际起主要约束作用的是**后者**；但当模型输出的函数恰好与另一条 needle 更像时（相似度可能很低但仍排第一），该题会记为失败（`wrong_target`）。因此该指标比单纯的 BLEU 阈值更严格，也比"真实仓库检索"的难度口径更宽松。

## 5. 使用方式（CLI）

安装后通过两个入口使用：`search_needle_function` 负责跑模型并落盘结果，`compute_score` 负责重算分数。

```bash
# 1) 用指定模型/后端执行 SNF 检索，结果写入 results/
repoqa.search_needle_function --model "gpt4-turbo" --backend openai   # 亦支持 anthropic / vllm / hf / google

# 2) 仅重算分数（不重新调用模型），用于调整阈值或复现榜单
repoqa.compute_score --model-output-path={model-output}.jsonl
```

输出路径约定：

| 产物 | 路径 |
|------|------|
| 原始输出 | `results/ntoken_{code-context-size}/{model}.jsonl` |
| 打分结果 | `results/ntoken_{code-context-size}/{model}-SCORE.json` |

其中 `{code-context-size}` 默认为 `16384`，因此默认目录是 `results/ntoken_16384/`。这种"原始输出 + 独立打分"的拆分意味着：修改阈值、重新实现相似度计算，都可以在不重复调用（付费）模型的前提下完成。

## 6. 官方榜单（16K 上下文，thresh = 0.8）

**榜单不在 GitHub README 里**，而托管在项目 homepage：<https://evalplus.github.io/repoqa.html> 。页面共 **33 行**（第 22 位并列两名），列序为 `.py / .cpp / .rs / .java / .ts / Avg`，相似度阈值 `thresh = 0.8`。榜单单据只有这 5 种语言、**没有 go 列**（尽管 release 数据含 go 的 100 条 needle，见 [第 3 节](#3-数据构成)口径说明），因此本表不能直接与含 go 的 600 题口径互换。

| # | Model | 宣称上下文 | Py | C++ | Rust | Java | TS | Avg |
|---|-------|-----------|----|-----|------|------|----|-----|
| 🥇 | gpt-4o-2024-05-13 | 128k | 95 | 80 | 85 | 96 | 97 | 90.6 |
| 🥇 | gemini-1.5-pro-latest | 1000k | 91 | 81 | 91 | 94 | 96 | 90.6 |
| 🥇 | claude-3-opus-20240229 | 200k | 93 | 83 | 88 | 95 | 94 | 90.6 |
| 4 | gemini-1.5-flash-latest | 1000k | 93 | 79 | 87 | 94 | 97 | 90.0 |
| 5 | claude-3-sonnet-20240229 | 200k | 88 | 81 | 85 | 92 | 91 | 87.4 |
| 6 | DeepSeek-V2-Chat | 128k | 90 | 76 | 77 | 91 | 83 | 83.4 |
| 7 | Meta-Llama-3-70B-Instruct | 8k | 83 | 70 | 81 | 86 | 91 | 82.2 |
| 8 | claude-3-haiku-20240307 | 200k | 80 | 75 | 74 | 90 | 90 | 81.8 |
| 9 | c4ai-command-r-plus | 128k | 81 | 74 | 76 | 84 | 77 | 78.4 |
| 10 | gpt-4-turbo-2024-04-09 | 128k | 84 | 79 | 75 | 89 | 55 | 76.4 |
| 11 | Mixtral-8x7B-Instruct-v0.1 | 32k | 66 | 65 | 64 | 71 | 74 | 68.0 |
| 12 | Mixtral-8x22B-Instruct-v0.1 | 64k | 60 | 67 | 74 | 83 | 55 | 67.8 |
| 13 | Qwen1.5-72B-Chat | 32k | 62 | 60 | 68 | 75 | 70 | 67.0 |
| 14 | Phi-3-medium-128k-instruct | 128k | 57 | 54 | 62 | 69 | 74 | 63.2 |
| 15 | CodeQwen1.5-7B-Chat | 64k | 69 | 47 | 57 | 74 | 67 | 62.8 |
| 16 | Mistral-7B-Instruct-v0.3 | 32k | 61 | 57 | 51 | 61 | 80 | 62.0 |
| 17 | gpt-3.5-turbo-0125 | 16k | 43 | 65 | 60 | 76 | 58 | 60.4 |
| 18 | Meta-Llama-3-8B-Instruct | 8k | 54 | 48 | 51 | 53 | 62 | 53.6 |
| 19 | deepseek-coder-33b-instruct | 16k | 59 | 44 | 23 | 53 | 63 | 48.4 |
| 20 | Mistral-7B-Instruct-v0.2 | 32k | 38 | 50 | 44 | 45 | 60 | 47.4 |
| 21 | CodeLlama-13b-Instruct-hf | 16k | 45 | 30 | 31 | 50 | 57 | 42.6 |
| 22 | DeepSeek-V2-Lite-Chat | 32k | 39 | 37 | 45 | 41 | 46 | 41.6 |
| 22 | CodeLlama-34b-Instruct-hf | 16k | 41 | 31 | 53 | 40 | 43 | 41.6 |
| 24 | Phi-3-small-128k-instruct | 128k | 25 | 48 | 30 | 46 | 49 | 39.6 |
| 25 | Qwen1.5-32B-Chat | 32k | 36 | 28 | 25 | 32 | 48 | 33.8 |
| 26 | CodeLlama-7b-Instruct-hf | 16k | 20 | 41 | 22 | 25 | 33 | 28.2 |
| 27 | Qwen1.5-14B-Chat | 32k | 4 | 30 | 26 | 36 | 34 | 26.0 |
| 28 | Magicoder-S-DS-6.7B | 16k | 27 | 21 | 7 | 25 | 36 | 23.2 |
| 29 | Phi-3-mini-128k-instruct | 128k | 19 | 25 | 25 | 21 | 22 | 22.4 |
| 30 | Mistral-7B-Instruct-v0.1 | 32k | 10 | 9 | 10 | 11 | 15 | 11.0 |
| 31 | deepseek-coder-6.7b-instruct | 16k | 11 | 21 | 2 | 3 | 16 | 10.6 |
| 32 | Qwen1.5-7B-Chat | 32k | 1 | 6 | 2 | 2 | 3 | 2.8 |
| 33 | codegemma-7b-it | 8k | 3 | 2 | 1 | 1 | 4 | 2.2 |

**榜单观察**

- **前三名同为 90.6，但排序不同**：官方首页把 `gpt-4o-2024-05-13` 列第一，论文则把 `claude-3-opus-20240229` 列第一。三者的 Avg 完全一致，差异只出现在子项（例如 TS 列 gpt-4o 为 97、claude-3-opus 为 94），因此"谁是第一"取决于排序口径，而非分数差异。
- **榜单与论文 Table 2 有 4 处 1 分之差**，均为榜单比论文高 1 分：
  - `Phi-3-medium-128k-instruct` 的 `.py`：榜单 57 / 论文 56
  - `CodeQwen1.5-7B-Chat` 的 `.rs`：榜单 57 / 论文 56
  - `Mistral-7B-Instruct-v0.3` 的 `.cpp`：榜单 57 / 论文 56
  - `CodeLlama-13b-Instruct-hf` 的 `.ts`：榜单 57 / 论文 56
  引用分数时应注明来源（homepage 或论文），不要把两个版本混用。
- **第 22 位为并列**：`DeepSeek-V2-Lite-Chat` 与 `CodeLlama-34b-Instruct-hf` 的 Avg 均为 41.6，因此 33 行数据只对应 32 个名次段。
- **榜单自 2024 年下半年起未更新**，其中没有 2025 年发布的新模型；需要新模型的成绩只能自行运行评测。

## 7. 论文主要结论

论文给出三条 takeaway（原文）：

> "(i) there is still a small gap between the best open and proprietary models; (ii) different models are good at different languages; and (iii) models may understand code better without comments."

围绕这三条，可以归纳出以下结论：

- **第一梯队几乎全是闭源模型**：Avg > 90% 的四款模型（`gpt-4o-2024-05-13` 90.6、`gemini-1.5-pro-latest` 90.6、`claude-3-opus-20240229` 90.6、`gemini-1.5-flash-latest` 90.0）均为闭源 API 模型。
- **开源模型已逼近但仍有差距**：最强开源模型 `DeepSeek-V2-Chat` 得 83.4、`Meta-Llama-3-70B-Instruct` 得 82.2，二者已略超 `claude-3-haiku-20240307`（81.8）与 `gpt-4-turbo-2024-04-09`（76.4），但仍未进入 90% 梯队。
- **语言难度梯度**：Java、TypeScript 最易 → Python → C++ → Rust 最难。作者推测这与训练语料中各语言代码量的多少相关。以三款模型的分项为例（顺序为 Py / C++ / Rust / Java / TS）：`gpt-4o-2024-05-13` 为 95 / 80 / 85 / 96 / 97，`gpt-4-turbo-2024-04-09` 为 84 / 79 / 75 / 89 / 55，`codegemma-7b-it` 为 3 / 2 / 1 / 1 / 4。即便是同一个模型，C++/Rust 也普遍低于 Java/TS；而弱模型在 Rust 上的下滑往往最剧烈（如 `deepseek-coder-33b-instruct` 的 Rust 只有 23，而 Py 有 59）。
- **注释消融**：除 Gemini 系以外，多数模型在"无注释"模式下反而提升，其中 `gpt-4o` 从 90.6 提升到 **93.2**，幅度最明显。这说明注释既可能是线索也可能是干扰。
- **典型失败模式**：`Gemini-1.5-Flash` 的典型失败是"忘掉任务、转而去数行号"——模型没有输出函数，而是开始列举行号，属于指令跟随失效而非检索能力不足。
- **短上下文模型的免训练解锁**：对上下文窗口只有 8k / 16k 的模型，用 dynamic RoPE scaling 或直接改写最大长度并取最优配置，即可在不做任何训练的情况下跑 16K 设置；作者据此认为"Llama-3-Instruct 家族其实是被隐藏的长上下文模型"。
- **作者自陈的局限**（来自官方 FAQ，论文本身没有 Limitations 节）：函数描述被刻意写得啰嗦，以避免一段描述同时匹配多个函数；这与真实场景中开发者常用短查询的习惯存在分布差异，因此 SNF 分数不能直接等同于真实代码检索体验。
- **Future work**：扩展 SNF（更多任务形态）、构造更复杂的任务、覆盖更多语言与模型。

## 8. 常见误传与澄清

| 传闻 | 核实结果 |
|------|----------|
| arXiv 编号是 2406.06036 | ❌ 2406.06036 是一篇无关的数学论文；正确编号为 **2406.06025** |
| 评测覆盖 2K–2M tokens 的变长上下文 | ❌ 论文与官网只有 **16K 固定窗口**，没有变长评测 |
| 存在 Go 语言子集 | ✅ **确实存在**（实测口径，此前判为"误传"是错的）。官方 release `repoqa-2024-06-23` 顶层 keys 为 `['python','cpp','java','typescript','rust','go']`，`go` 含 10 仓库 × 10 needle = **100 条**。论文/README 只写 5 语言是**写作时未含 go**，后续 release 扩充 |
| 存在 `repoqa-2024-06-23` / `repoqa-2024-06-24` 等数据集版本 | ✅ `repoqa-2024-06-23` **确实存在**：它就是官方 `repoqa/data.py` 中 `REPOQA_DATA_VERSION` 的默认值，也是 `evalplus/repoqa_release` 的 GitHub Release tag（下载地址见 [第 10 节](#10-参考链接)）。`2024-06-24` 这一 tag 本次未核实 |
| README 里就有 leaderboard 表格 | ❌ README 只有安装、用法与引用信息；榜单在 homepage（`evalplus.github.io/repoqa.html`） |
| 判分使用 LLM-as-judge | ❌ 判定为 tree-sitter 解析 + 平滑 BLEU ≥ 0.8 且排序第一，全程没有 LLM 评审 |

> 上表中「存在 Go 语言子集」与「存在 `repoqa-2024-06-23` 等数据集版本」两条此前被列为"误传"，2026-11-11 下载官方 release 数据并复刻 `compute_score.py` 判分后**已推翻**：错因是当时只查了 README、arXiv 全文与 HuggingFace 组织页，而数据集本身既不在仓库里、也不在 HF 上，只存在于 `evalplus/repoqa_release` 的 GitHub Release 二进制中。

## 9. 维护状态

| 指标 | 数值 |
|------|------|
| Stars | 137 |
| Forks | 10 |
| Watchers | 5 |
| Releases | 7 个，最新为 v0.1.2 |
| 最新 commit | `ae876de`，时间 2024-10-29，信息 `refactor(hf): use stop_strings over external lib` |
| 许可证 | Apache-2.0 |
| 数据集获取方式 | 不在 HuggingFace 上托管；由 `repoqa/data.py` 从 `evalplus/repoqa_release` 的 GitHub Release 下载（默认 tag = `REPOQA_DATA_VERSION` = `2024-06-23`） |

仓库的最新 commit 停留在 2024-10-29，榜单也未增补 2025 年的模型，可视为进入低频维护状态。若要用于新模型的横向对比，需要按 [第 5 节](#5-使用方式cli) 的 CLI 自行运行评测，并注意保持 `--code-context-size` 与 tokenizer 设置与官方一致，否则分数不可比。

## 10. 参考链接

| 资源 | 链接 |
|------|------|
| 论文摘要页（arXiv abs） | <https://arxiv.org/abs/2406.06025> |
| 论文全文（arXiv HTML，v1） | <https://arxiv.org/html/2406.06025v1> |
| GitHub 仓库 | <https://github.com/evalplus/repoqa> |
| **数据集下载（实测可用）** | <https://github.com/evalplus/repoqa_release/releases/download/2024-06-23/repoqa-2024-06-23.json.gz>（**12.5 MB gz**，HTTP 200 / `12525636` B，解压后 **71,510,991 B**） |
| 官方榜单页 | <https://evalplus.github.io/repoqa.html> |
| EvalPlus 组织页 | <https://github.com/evalplus> |
| HF evalplus 组织页 | <https://huggingface.co/evalplus>（**无** RepoQA 数据集） |

表中 arXiv HTML 链接指向 v1 的 HTML 版本；该 HTML 页面的可访问性本次**未逐次核实**，若链接失效，请以摘要页提供的 PDF 为准。

## 11. 信息核实说明（含常见误传核实与修订记录）

**采集时间与方式**

- 采集时间：2026-11-11。
- 采集方式：通过真实 Chrome 直连打开官方一手页面取证，共两轮；榜单、论文摘要、README 均以渲染后的页面文本为准，未依赖二手博客或搜索摘要。
- 证据截图：
  - 榜单：`/Users/ydl/.yagent/data/browser/5315c972-6383-407d-a5d6-55f0b07c/repoqa-leaderboard-16k.png`
  - 论文摘要：`/Users/ydl/.yagent/data/browser/5315c972-6383-407d-a5d6-55f0b07c/repoqa-arxiv-abstract.png`
  - 同一榜单页另有 `repoqa-leaderboard.png`，它与 `repoqa-leaderboard-16k.png` 是同一页面两次截取（尺寸同为 756x469，字节数均为 68588 B）。
  - 说明：本次取证会话的 task id 为 `...55f0b07c7fde`，而磁盘上实际保存截图的目录名是 `...55f0b07c`（相对 task id 截断了 `7fde` 后缀）；以上路径均为实测存在的文件（每张均为 756x469 视口截图，非整页长图）。

**网络说明**

`github.com`、`arxiv.org`、`evalplus.github.io` 均可直连成功，未使用代理；仅搜索引擎路径不可用（Bing 国内版结果被污染、DuckDuckGo 触发 CAPTCHA），因此采用"已知权威 URL 直接打开"的取证策略，而不是先搜索再点击。

**未核实项**

- **2K–2M 变长上下文数据**：论文与官网均只有 16K 固定窗口，未发现任何变长评测的证据。
- **needle 深度分层性能曲线**：数据集按 0.05–0.95 十个档位植入针函数，但论文与官网都没有给出"分数随深度变化"的曲线；本仓库中也未找到该数据的公开出处。
- **Go 语言成绩**：官方 homepage 榜单与论文 Table 2 只报 5 种语言、**没有 go 列**，因此 go 的"官方成绩"仍无一手依据；但 go 的**数据子集确实存在**（官方 release 含 100 条 go needle），要 go 的成绩只能自行运行评测。
- **2025 年后的新模型成绩**：榜单未更新，官方未提供相应数据，需自行运行评测。
- **"Repository Question Answering" 作为正式全称**：论文标题与摘要只写 RepoQA，未做全称展开；该说法属社区按字面理解，非官方定义。

**关于"常见误传"的进一步说明**

[第 8 节](#8-常见误传与澄清)列出的六条传闻中，四条仍是在检索过程中反复出现、但无法在一手页面中找到依据的说法（arXiv 编号、变长上下文、README 榜单、LLM-as-judge）；另两条（go 子集、`repoqa-2024-06-23` 版本）经 2026-11-11 的数据实测**已确认属实**，原来的"误传"判定是取证范围不足造成的误判。其中最容易被误引的是 arXiv 编号（`2406.06036` 与 `2406.06025` 仅差一位）与"变长上下文"——后者很可能是把其他长上下文基准的设定张冠李戴到了 RepoQA 上。引用本文档中的任何分数时，请同时注明来源是 homepage 榜单还是论文 Table 2，因为两者存在 4 处 1 分之差。

**修订记录（2026-11-11，基于官方数据实测）**

依据：宿主机下载了官方 release `repoqa-2024-06-23.json.gz`（`12525636` B，解压 `71,510,991` B），并用**官方 `repoqa/compute_score.py` + `metric.py` 的逻辑复刻**（BLEU-4 / Chen & Cherry method4 / 候选集=该 repo 的 10 条 needle / threshold 0.8）对 codeseek 的真实检索结果判分后逐项核对，推翻/修正了以下 4 处（均为**论文/文档口径**与**代码/数据实测口径**的差异，原文说法保留并标注）：

1. **Go 语言子集确实存在**：数据顶层 keys 为 `['python','cpp','java','typescript','rust','go']`，`go` 含 10 仓库 × 10 needle = 100 条。第 8 节原判"❌ 无任何依据"已改为 ✅，第 11 节"5 种语言中不含 Go"已改正。
2. **测试数量 500 → 实为 600**：论文与 README 的 `500 = 5×10×10` 是写作时口径；release 实测为 `6×10×10 = 600`（第 3 节表格已并列两个口径）。
3. **needle 深度 10%–100% → 实为 0.05–0.95**：`search_needle_function.py` 取 `position_ratio = (i + 0.5) / len(needles)`（i=0..9），即 5% 步长、两端各留半档（第 3 节「针函数位置」行已改）。
4. **候选集不是"全仓库 tree-sitter 解析"**：`compute_score.py` 的候选集就是该 repo 的 10 条 needle 按 `start_line:end_line` 切片；tree-sitter 只用于从**模型输出**中取第一个函数，不参与构建候选池（第 4 节已改，并补充"判定比与全仓库函数竞争宽松得多"的影响说明）。

> 附带结论：`repoqa-2024-06-23` 也确实是 `data.py` 中 `REPOQA_DATA_VERSION` 的默认 tag，第 10 节已补上实测可用的下载地址。

## 12. codeseek 自身实测（本地 100 题子集，非官方榜）

> 本节记录**本项目**（codeseek 检索器）在该基准上的实测成绩，与第 6 节的官方 LLM 榜单**不同口径、不可比较**：官方榜测的是"把 16K 仓库上下文塞给大模型、由模型输出函数"，本节测的是"用 codeseek 的 `search` 检索、取 Top-1 的 `code_block` 判分"。完整归档见 [`evals/repoqa/results/README.md`](../evals/repoqa/results/README.md)。

| 项 | 取值 |
|----|------|
| 子集 | 100 题 = 5 语言（python / cpp / java / typescript / rust）× 2 仓库 × 10 needle，**不含 go** |
| 数据集 | 官方 release `2024-06-23`，`sha256` 前缀 `bd3f7cab47283cde` |
| 判分 | 官方逻辑复刻：BLEU-4 + Chen & Cherry `method4`，候选集=该 repo 的 10 条 needle，`threshold = 0.8` |
| 检索 | `hybrid` 100%（LanceDB + BM25），`search_limit=5`，判分取 Top-1，0 空结果 |
| 被测二进制 | `rust-core/target/release/codeseek`，`sha256` 前缀 `b1532bb1b43e3d65` |

**当前主口径：总体 84.0%（84/100）**

| 语言 | 通过/总数 | accuracy | 与上一轮（82.0%）相比 |
|------|-----------|----------|----------------------|
| python | 18/20 | 90.0% | 持平 |
| cpp | 11/20 | **55.0%** | **+2 题（45.0% → 55.0%）** |
| java | 17/20 | 85.0% | 持平 |
| typescript | 20/20 | 100.0% | 持平 |
| rust | 18/20 | 90.0% | 持平 |
| **all** | **84/100** | **84.0%** | **+2.0pt** |

两轮改进路径（均在 `rust-core/` 内，切块与排序分离）：

1. **修复前 74.0% → 上一轮 82.0%（rust）**：`treesitter/parsers/rust.rs` 的 `parse_block` 补 `mod_item` 分支（此前 `mod tests` 子树零产出）+ `impl_item` 的 `full_range` 收窄到 `declaration_range`，rust 上一轮前的 50.0% → 上一轮的 90.0%（+8.0pt，8 题翻转、0 回归）。
2. **上一轮 82.0% → 当前 84.0%（cpp）**：`services/hybrid_search.rs` 的 `demote_enclosing_candidates` —— **排序期**父子包含降权，语言白名单 `ENCLOSING_DEMOTE_LANGUAGES = ["cpp"]`。即**已通过排序期包含降权（仅 cpp）把 cpp 从 45.0% 提升到 55.0%，整体 82.0% → 84.0%；切块层内容降级方案已实测否决（78.0%）**（见下表）。

**已实测否决的方案（避免重走，均为清索引全量重跑）**

| 方案 | 做法 | 实测 | 结论 |
|------|------|------|------|
| 切块层内容降级 | 父容器 chunk 的 `content` 改为 `declaration_range` 签名骨架 | 78.0%（cpp 55%，但 typescript 100%→70%、rust 90%→85%） | **否决**：容器退化成"超短文档 + 符号名完美命中"，BM25 长度归一化把它顶到 rank1，正文只剩签名 → BLEU≈0 |
| 全语言排序期包含降权 | 降权对全部语言生效 | 81.0%（cpp 55%、java 90%，但 typescript 85%、rust 85%） | **否决**：typescript 的 needle 常是 class 本身、rust 的正确块被 60 行窗口 span 误判 |
| BM25 存真实 `line_end` | tantivy schema 增 STORED `line_start`/`line_end` 并按边界回填 | 48.0%（rust 全 `unknown_empty`、express 10 题全空） | **回退**：按 `line_end` 切片产生空正文，已 `git revert` |
| **排序期包含降权（仅 cpp）** | 同上 + `cpp` 门控 | **84.0%（cpp 55%，其余零回归）** | **采用**（当前主口径） |

关于 cpp 的现状：本轮之后 cpp 的瓶颈**已从"切块粒度"转为"召回/排序"**——剩余 9 道 cpp 失败题中仅 2 道与 needle 同名（`below_threshold`），另 7 道为 `wrong_target`（返回了别的符号）。因此"cpp 是最大短板、建议做 chunker 包含剪枝"这条旧结论**已不再成立**：已通过**排序期**父子包含降权（语言门控仅 cpp）把 cpp 由 45.0% 提升至 55.0%，总体 82.0%→84.0%；而切块层内容降级方案实测 78.0% 已否决（会制造"短文档 + 符号名完美命中"的诱饵抢 rank1，正文只剩签名 → BLEU≈0）。后续应从 cpp 符号覆盖（宏、模板特化、匿名 namespace 内静态函数）与同名符号排序入手。

> 口径提醒：本节分数与第 6 节官方榜（gpt-4o / claude-3-opus 等 90.6）**不可直接对比**，也不构成"codeseek 强于某 LLM"的结论——任务形态（检索 vs 长上下文生成）、候选集（10 条 needle vs 全仓库）与输入（源文件快照 vs 16K 依赖排序上下文）都不同。
