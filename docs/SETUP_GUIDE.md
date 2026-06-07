# CodeSeek Setup Guide — Embedding Model Configuration

## Why do I need an embedding model?

CodeSeek has two search modes:

| Mode | How it works | Requires model? |
|------|-------------|:--:|
| **Graph-based name search** | Matches function/symbol names against your query | No |
| **Semantic vector search** | Converts both your query and every code block into vectors, finds semantically similar code via cosine similarity | Yes |

Without an embedding model, you can still:
- Search for exact/matching symbol names (`codeseek search main`)
- Traverse call graphs (`codeseek callers`, `codeseek callees`)
- Check index status (`codeseek status`)

With an embedding model, you gain:
- Natural language queries (`"find authentication middleware"`)
- Cross-language search (find Python patterns even when searching in English)
- Fuzzy matching (typos, synonyms, intent-based search)

**You don't need an embedding model to get started.** You can skip configuration and add it later by re-running `codeseek`.

## How to get an embedding model

### Option 1: SiliconFlow (Recommended — affordable, easy setup)

SiliconFlow offers a free tier with 50M tokens/month, more than enough for personal use.

1. Visit [https://cloud.siliconflow.cn/account/ak](https://cloud.siliconflow.cn/account/ak)
2. Register with your phone number or email
3. Click **Create API Key**
4. Copy the key (format: `sk-xxxxxxxxxxxxxxxxxxxxxxxx`)
5. In the CodeSeek setup wizard, enter:
   - API Base URL: `https://api.siliconflow.cn/v1`
   - Model: `Qwen/Qwen3-Embedding-4B`
   - API Token: paste your key
   - Dimensions: `2560`

**Pricing (as of 2026):**
- Free tier: 50M tokens/month
- Paid: ¥0.7 / 1M tokens (~$0.10)
- Indexing a 10,000-function project costs ~¥0.02

### Option 2: OpenAI

1. Visit [https://platform.openai.com/api-keys](https://platform.openai.com/api-keys)
2. Create a new secret key
3. In the CodeSeek setup wizard, enter:
   - API Base URL: `https://api.openai.com/v1`
   - Model: `text-embedding-3-small`
   - API Token: paste your OpenAI key
   - Dimensions: `1536`

**Pricing (as of 2026):**
- `text-embedding-3-small`: $0.02 / 1M tokens
- Requires a funded OpenAI account (prepaid credits)

### Option 3: Any OpenAI-compatible provider

CodeSeek supports any API that implements the OpenAI `/v1/embeddings` endpoint. Just enter the appropriate base URL, model name, dimensions, and token.

Common compatible providers:
- [Together AI](https://together.ai) — `https://api.together.xyz/v1`
- [Groq](https://groq.com) — `https://api.groq.com/openai/v1`
- [Ollama](https://ollama.com) (local) — `http://localhost:11434/v1`

## How to configure

### First-time setup (recommended)

Just run `codeseek` with no arguments:

```bash
codeseek
```

The wizard will guide you through provider selection, model choice, and token entry.

### Manual configuration

Create or edit `~/.codeseek/config.json`:

```json
{
  "embedding": {
    "provider": "openai-compatible",
    "model": "Qwen/Qwen3-Embedding-4B",
    "api_token": "sk-your-token-here",
    "api_base_url": "https://api.siliconflow.cn/v1",
    "dimensions": 2560
  },
  "index": {
    "min_code_block_length": 16
  }
}
```

Then run `codeseek init` to build the index with embeddings.

## Model dimension reference

Different models output vectors of different dimensions. Use the correct value:

| Model | Dimensions |
|-------|-----------|
| Qwen/Qwen3-Embedding-4B | 2560 |
| Qwen/Qwen3-Embedding-8B | 4096 |
| BAAI/bge-large-zh-v1.5 | 1024 |
| text-embedding-3-small | 1536 |
| text-embedding-3-large | 3072 |

## Troubleshooting

**Q: `codeseek init` takes forever?**

The first run embeds every function in your codebase. For a 100k-line project it may take 1-2 minutes. Subsequent runs are incremental — only changed files are re-embedded.

**Q: Search returns empty results?**

Check that:
1. `codeseek status` shows a non-zero function count
2. `~/.codeseek/config.json` has a valid `api_token`
3. Your API provider is accessible (try `curl` to the base URL)

**Q: I don't want to use embeddings at all?**

That's fine. CodeSeek falls back to graph-based name search automatically when embeddings are unavailable. Just don't configure the embedding model.
