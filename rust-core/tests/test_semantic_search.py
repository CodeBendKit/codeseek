#!/usr/bin/env python3
"""
CodeSeek Semantic Search 功能测试脚本

测试 codebase 语义搜索接口（运行在本地 12800 端口）的准确性，
特别关注混合检索（Hybrid Search）与纯 Embedding 检索的对比分析。

版本: 2.0
修改日志:
  v2.0 (2025-07-14):
    - [修复] stability_analysis 现在返回 all_stats 列表，修复 print_conclusions 参数类型错误
    - [修复] main 函数正确捕获稳定性分析返回值
    - [增强] 新增 compare_hybrid_detail(): 精确符号匹配可视化，标记 ★
    - [增强] 新增 score_distribution_analysis(): 分数百分位数、相邻差异分析
    - [增强] 新增 RELEVANCE_JUDGMENTS 和 calculate_mrr(): 手动标注和 MRR 评估
    - [增强] 新增 detect_problematic_queries(): 自动检测问题查询
    - [增强] conclusions 部分集成所有增强功能

用法:
    python test_semantic_search.py

依赖:
    pip install requests tabulate colorama
"""

import json
import math
import statistics
import sys
import time
import traceback
from collections import defaultdict
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass, field
from typing import Optional

import requests

# ---------------------------------------------------------------------------
# 配置
# ---------------------------------------------------------------------------
BASE_URL = "http://127.0.0.1:12800"
REQUEST_TIMEOUT = 10  # 秒
MAX_RETRIES = 2
DEFAULT_LIMIT = 10

# ---------------------------------------------------------------------------
# ANSI 彩色输出
# ---------------------------------------------------------------------------
class Colors:
    """ANSI 颜色代码"""
    HEADER = "\033[95m"
    BLUE = "\033[94m"
    CYAN = "\033[96m"
    GREEN = "\033[92m"
    YELLOW = "\033[93m"
    RED = "\033[91m"
    BOLD = "\033[1m"
    UNDERLINE = "\033[4m"
    RESET = "\033[0m"
    DIM = "\033[2m"

def cprint(text: str, color: str = "", bold: bool = False):
    """彩色打印"""
    prefix = Colors.BOLD if bold else ""
    print(f"{prefix}{color}{text}{Colors.RESET}")

def print_separator(char: str = "━", length: int = 70):
    """打印分隔线"""
    print(Colors.DIM + char * length + Colors.RESET)

def print_section(title: str):
    """打印章节标题"""
    print()
    cprint(f"  {title}", Colors.BOLD + Colors.CYAN, bold=True)
    print_separator("━", 70)

# ---------------------------------------------------------------------------
# 数据结构
# ---------------------------------------------------------------------------
@dataclass
class SearchResult:
    """搜索结果行"""
    rank: int
    file_path: str
    symbol_name: str
    score: float
    symbol_type: str
    language: str
    line_start: int = 0
    line_end: int = 0

@dataclass
class QueryResult:
    """单次查询的完整结果"""
    query: str
    limit: int
    elapsed: float
    results: list  # list[SearchResult]
    success: bool
    error: str = ""
    raw_response: dict = field(default_factory=dict)

# ---------------------------------------------------------------------------
# HTTP 工具函数
# ---------------------------------------------------------------------------
def api_request(method: str, path: str, payload: Optional[dict] = None, timeout: int = REQUEST_TIMEOUT) -> dict:
    """发送 HTTP 请求，带重试逻辑"""
    url = f"{BASE_URL}{path}"
    last_error = None
    
    for attempt in range(MAX_RETRIES + 1):
        try:
            if method.upper() == "GET":
                resp = requests.get(url, timeout=timeout)
            elif method.upper() == "POST":
                resp = requests.post(url, json=payload, timeout=timeout)
            else:
                raise ValueError(f"不支持的 HTTP 方法: {method}")
            
            resp.raise_for_status()
            return resp.json()
        except requests.exceptions.ConnectionError as e:
            last_error = f"连接失败 (尝试 {attempt + 1}/{MAX_RETRIES + 1}): {e}"
            if attempt < MAX_RETRIES:
                time.sleep(1)
        except requests.exceptions.Timeout as e:
            last_error = f"请求超时 (尝试 {attempt + 1}/{MAX_RETRIES + 1}): {e}"
            if attempt < MAX_RETRIES:
                time.sleep(1)
        except requests.exceptions.HTTPError as e:
            last_error = f"HTTP 错误: {e}"
            break  # HTTP 错误不重试
        except Exception as e:
            last_error = f"未知错误: {e}"
            break
    
    raise Exception(last_error)

def check_health() -> dict:
    """健康检查"""
    health = {}
    try:
        data = api_request("GET", "/health")
        health["/health"] = {"status": "OK", "code": 200, "data": data}
        cprint("  ✓ GET /health — OK (200)", Colors.GREEN)
    except Exception as e:
        health["/health"] = {"status": "FAIL", "error": str(e)}
        cprint(f"  ✗ GET /health — {e}", Colors.RED)
    
    try:
        data = api_request("GET", "/status")
        health["/status"] = {"status": "OK", "code": 200, "data": data}
        cprint("  ✓ GET /status — OK (200)", Colors.GREEN)
    except Exception as e:
        health["/status"] = {"status": "FAIL", "error": str(e)}
        cprint(f"  ✗ GET /status — {e}", Colors.RED)
    
    return health

def semantic_search(query: str, limit: int = DEFAULT_LIMIT) -> QueryResult:
    """执行语义搜索"""
    start = time.time()
    try:
        payload = {"text": query, "limit": limit}
        data = api_request("POST", "/semantic_search", payload)
        elapsed = time.time() - start
        
        results = []
        if data.get("success") and data.get("data"):
            for i, r in enumerate(data["data"]["results"]):
                results.append(SearchResult(
                    rank=i + 1,
                    file_path=r.get("file_path", ""),
                    symbol_name=r.get("symbol_name", ""),
                    score=r.get("score", 0.0),
                    symbol_type=r.get("symbol_type", ""),
                    language=r.get("language", ""),
                    line_start=r.get("line_start", 0),
                    line_end=r.get("line_end", 0),
                ))
        elif data.get("success") and not data.get("data"):
            elapsed = time.time() - start
            return QueryResult(
                query=query, limit=limit, elapsed=elapsed,
                results=[], success=True, raw_response=data
            )
        
        return QueryResult(
            query=query, limit=limit, elapsed=elapsed,
            results=results, success=True, raw_response=data
        )
    except Exception as e:
        elapsed = time.time() - start
        return QueryResult(
            query=query, limit=limit, elapsed=elapsed,
            results=[], success=False, error=str(e)
        )

# ---------------------------------------------------------------------------
# 查询集定义
# ---------------------------------------------------------------------------
QUERY_SETS = {
    "概念搜索（语义理解）": [
        "error handling",
        "user authentication",
        "concurrency control",
        "database operations",
        "HTTP server routing",
    ],
    "精确符号搜索（关键词匹配）": [
        "semantic_search",
        "HybridSearchService",
        "EmbeddingService",
        "TantivyBm25Index",
        "reciprocal_rank_fusion",
    ],
    "混合查询（语义 + 精确）": [
        "code chunk indexing",
        "search function implementation",
        "embedding cache provider",
        "RRF score calculation",
        "HTTP handler for search",
    ],
    "短查询（1-2个词）": [
        "auth",
        "search",
        "embed",
        "token",
        "config",
    ],
}

# ---------------------------------------------------------------------------
# 相关性标注（用于 MRR 评估）
# ---------------------------------------------------------------------------
# 定义格式: {查询文本: {"expected_symbols": [期望出现的符号名列表]}}
# 如果为空字典，则跳过 MRR 计算
# 示例:
#   RELEVANCE_JUDGMENTS = {
#       "semantic_search": {"expected_symbols": ["semantic_search", "SemanticSearchRequest"]},
#       "HybridSearchService": {"expected_symbols": ["HybridSearchService"]},
#   }
RELEVANCE_JUDGMENTS: dict = {
    # 可以在这里添加你期望的精确匹配结果
}

# ---------------------------------------------------------------------------
# 结果展示
# ---------------------------------------------------------------------------
def format_result_table(query_result: QueryResult, max_name_width: int = 40, max_file_width: int = 35):
    """格式化单个查询的结果表格"""
    lines = []
    q = query_result.query
    if len(q) > 40:
        q = q[:37] + "..."
    lines.append(f"\n  查询: \"{query_result.query}\" (limit={query_result.limit}, 耗时 {query_result.elapsed:.2f}s)")
    print_separator("─", 70)
    
    if not query_result.success:
        cprint(f"  ✗ 查询失败: {query_result.error}", Colors.RED)
        return lines
    
    if not query_result.results:
        cprint("  (无结果)", Colors.DIM)
        return lines
    
    # 表头
    header = f"  {'#':<3} │ {'文件路径':<{max_file_width}} │ {'符号名':<{max_name_width}} │ {'分数':>7} │ {'类型':<10} │ {'语言':<6}"
    lines.append(header)
    lines.append("  " + "─" * (len(header) - 2))
    
    for r in query_result.results:
        fp = r.file_path
        if len(fp) > max_file_width:
            fp = "..." + fp[-(max_file_width - 3):]
        sn = r.symbol_name
        if len(sn) > max_name_width:
            sn = sn[:max_name_width - 3] + "..."
        
        row = (
            f"  {r.rank:<3} │ {fp:<{max_file_width}} │ {sn:<{max_name_width}} │ "
            f"{r.score:>7.4f} │ {r.symbol_type:<10} │ {r.language:<6}"
        )
        lines.append(row)
    
    return lines

def print_query_report(query_result: QueryResult):
    """打印单个查询的报告"""
    lines = format_result_table(query_result)
    for line in lines:
        print(line)
    
    # 统计摘要
    if query_result.results:
        scores = [r.score for r in query_result.results]
        langs = defaultdict(int)
        types = defaultdict(int)
        files = set()
        for r in query_result.results:
            langs[r.language] += 1
            types[r.symbol_type] += 1
            files.add(r.file_path)
        
        print_separator("─", 70)
        cprint("  统计摘要", Colors.BOLD)
        print(f"  结果数量: {len(query_result.results)}")
        print(f"  分数范围: {min(scores):.4f} ~ {max(scores):.4f} (平均 {sum(scores)/len(scores):.4f})")
        print(f"  去重文件: {len(files)} 个")
        if langs:
            lang_str = ", ".join(f"{k}: {v}" for k, v in sorted(langs.items(), key=lambda x: -x[1]))
            print(f"  语言分布: {lang_str}")
        if types:
            type_str = ", ".join(f"{k}: {v}" for k, v in sorted(types.items(), key=lambda x: -x[1]))
            print(f"  类型分布: {type_str}")

# ---------------------------------------------------------------------------
# 统计分析
# ---------------------------------------------------------------------------
@dataclass
class QueryStats:
    """查询统计"""
    query: str
    limit: int
    elapsed: float
    result_count: int
    min_score: float = 0.0
    max_score: float = 0.0
    avg_score: float = 0.0
    languages: dict = field(default_factory=dict)
    types: dict = field(default_factory=dict)
    unique_files: int = 0
    success: bool = False
    error: str = ""

def compute_stats(qr: QueryResult) -> QueryStats:
    """计算查询统计"""
    stats = QueryStats(query=qr.query, limit=qr.limit, elapsed=qr.elapsed,
                       result_count=len(qr.results), success=qr.success, error=qr.error)
    
    if not qr.results:
        return stats
    
    scores = [r.score for r in qr.results]
    stats.min_score = min(scores)
    stats.max_score = max(scores)
    stats.avg_score = sum(scores) / len(scores)
    
    for r in qr.results:
        stats.languages[r.language] = stats.languages.get(r.language, 0) + 1
        stats.types[r.symbol_type] = stats.types.get(r.symbol_type, 0) + 1
        stats.unique_files += 1
    stats.unique_files = len(set(r.file_path for r in qr.results))
    
    return stats

def print_global_statistics(all_results: list[QueryResult]):
    """打印全局统计"""
    print_section("全局统计报告")
    
    # 准备数据
    successful = [qr for qr in all_results if qr.success and qr.results]
    failed = [qr for qr in all_results if not qr.success]
    
    total_queries = len(all_results)
    total_results = sum(len(qr.results) for qr in successful)
    avg_time = sum(qr.elapsed for qr in successful) / len(successful) if successful else 0
    
    cprint(f"  总查询数: {total_queries}", Colors.BOLD)
    cprint(f"  成功查询: {len(successful)}", Colors.GREEN)
    if failed:
        cprint(f"  失败查询: {len(failed)}", Colors.RED)
    cprint(f"  总返回结果: {total_results}", Colors.BOLD)
    cprint(f"  平均响应时间: {avg_time:.2f}s", Colors.BOLD)
    
    # 响应时间排序
    if successful:
        print()
        cprint("  响应时间排序 (最快→最慢):", Colors.CYAN)
        sorted_by_time = sorted(successful, key=lambda qr: qr.elapsed)
        for i, qr in enumerate(sorted_by_time[:5], 1):
            q = qr.query[:30] + "..." if len(qr.query) > 30 else qr.query
            cprint(f"    {i}. {q:<30} {qr.elapsed:.2f}s", Colors.CYAN)
        if len(sorted_by_time) > 5:
            slowest = sorted_by_time[-3:]
            cprint(f"    ... 最慢的 3 个:", Colors.DIM)
            for i, qr in enumerate(slowest, 1):
                q = qr.query[:30] + "..." if len(qr.query) > 30 else qr.query
                cprint(f"      {i}. {q:<30} {qr.elapsed:.2f}s", Colors.DIM)
    
    # 分数分布汇总
    if successful:
        print()
        cprint("  各查询分数统计:", Colors.CYAN)
        table_data = []
        for qr in successful:
            scores = [r.score for r in qr.results]
            q = qr.query[:25] + "..." if len(qr.query) > 25 else qr.query
            table_data.append([
                q,
                len(qr.results),
                f"{min(scores):.4f}" if scores else "-",
                f"{max(scores):.4f}" if scores else "-",
                f"{sum(scores)/len(scores):.4f}" if scores else "-",
            ])
        
        try:
            from tabulate import tabulate
            headers = ["查询", "结果数", "最小分", "最大分", "平均分"]
            print(tabulate(table_data, headers=headers, tablefmt="grid"))
        except ImportError:
            for row in table_data:
                print(f"    {row[0]:<28} {row[1]:>5}  {row[2]:>8}  {row[3]:>8}  {row[4]:>8}")
    
    # 语言分布汇总
    all_langs = defaultdict(int)
    all_types = defaultdict(int)
    all_files = set()
    for qr in successful:
        for r in qr.results:
            all_langs[r.language] += 1
            all_types[r.symbol_type] += 1
            all_files.add(r.file_path)
    
    if all_langs:
        print()
        cprint("  综合语言分布:", Colors.CYAN)
        for lang, count in sorted(all_langs.items(), key=lambda x: -x[1]):
            bar = "█" * min(count, 30)
            cprint(f"    {lang:<15} {count:>4} {bar}", Colors.CYAN)
    
    if all_types:
        print()
        cprint("  综合类型分布:", Colors.CYAN)
        for typ, count in sorted(all_types.items(), key=lambda x: -x[1]):
            bar = "█" * min(count, 30)
            cprint(f"    {typ:<15} {count:>4} {bar}", Colors.CYAN)
    
    cprint(f"\n  去重文件总数: {len(all_files)}", Colors.BOLD)

# ---------------------------------------------------------------------------
# 混合检索稳定性分析
# ---------------------------------------------------------------------------
def stability_analysis(all_results: dict[str, dict[int, QueryResult]]):
    """
    混合检索稳定性分析
    
    策略: 对同一查询用不同 limit (5, 10, 20) 执行搜索，观察 Top-5 结果的一致性。
    如果 limit=5 和 limit=20 的 Top-5 完全一致，说明排序稳定；
    如果差异大，说明混合检索的 RRF 融合引入了噪声，导致排序不稳定。
    """
    print_section("对比分析: Hybrid 稳定性测试")
    
    cprint("  策略: 对同一查询用不同 limit (5, 10, 20) 搜索，观察 Top-5 结果的一致性", Colors.DIM)
    cprint("  一致性高 = 排序稳定  |  一致性低 = 混合检索可能引入噪声\n", Colors.DIM)
    
    all_stats = []
    
    for category, queries in QUERY_SETS.items():
        cprint(f"\n  ── {category} ──", Colors.BOLD)
        
        for query in queries:
            limits = all_results.get(query)
            if not limits:
                continue
            
            # 获取 limit=5 和 limit=20 的 Top-5 文件路径
            top5_limit5 = []
            top5_limit20 = []
            top5_limit10 = []
            
            if 5 in limits and limits[5].success:
                top5_limit5 = [r.file_path for r in limits[5].results[:5]]
            if 10 in limits and limits[10].success:
                top5_limit10 = [r.file_path for r in limits[10].results[:5]]
            if 20 in limits and limits[20].success:
                top5_limit20 = [r.file_path for r in limits[20].results[:5]]
            
            # 计算一致性百分比
            def calc_consistency(base, other):
                if not base or not other:
                    return 0.0
                base_set = set(base)
                other_set = set(other)
                common = base_set & other_set
                return len(common) / len(base_set) * 100
            
            consistency_5_20 = calc_consistency(top5_limit5, top5_limit20)
            consistency_10_20 = calc_consistency(top5_limit10, top5_limit20)
            consistency_5_10 = calc_consistency(top5_limit5, top5_limit10)
            
            # 判断
            verdict = "✓" if consistency_5_20 >= 80 else "⚠" if consistency_5_20 >= 50 else "✗"
            verdict_color = Colors.GREEN if consistency_5_20 >= 80 else Colors.YELLOW if consistency_5_20 >= 50 else Colors.RED
            
            q = query[:30] + "..." if len(query) > 30 else query
            cprint(f"  查询: \"{query}\"", Colors.BLUE)
            cprint(f"    limit=5  Top-5: {[f.split('/')[-1] for f in top5_limit5]}", Colors.DIM)
            cprint(f"    limit=10 Top-5: {[f.split('/')[-1] for f in top5_limit10]}", Colors.DIM)
            cprint(f"    limit=20 Top-5: {[f.split('/')[-1] for f in top5_limit20]}", Colors.DIM)
            cprint(f"    5 vs 10 一致性: {consistency_5_10:.0f}% | 5 vs 20: {consistency_5_20:.0f}% | 10 vs 20: {consistency_10_20:.0f}%", verdict_color, bold=True)
            
            all_stats.append({
                "query": query,
                "consistency_5_10": consistency_5_10,
                "consistency_5_20": consistency_5_20,
                "consistency_10_20": consistency_10_20,
            })
    
    # 汇总分析
    print()
    print_separator("━", 70)
    cprint("  稳定性分析汇总", Colors.BOLD + Colors.CYAN)
    
    if all_stats:
        avg_5_10 = sum(s["consistency_5_10"] for s in all_stats) / len(all_stats)
        avg_5_20 = sum(s["consistency_5_20"] for s in all_stats) / len(all_stats)
        avg_10_20 = sum(s["consistency_10_20"] for s in all_stats) / len(all_stats)
        
        cprint(f"  5 vs 10 平均一致性: {avg_5_10:.1f}%", Colors.CYAN)
        cprint(f"  5 vs 20 平均一致性: {avg_5_20:.1f}%", Colors.CYAN)
        cprint(f"  10 vs 20 平均一致性: {avg_10_20:.1f}%", Colors.CYAN)
        
        # 判断
        if avg_5_20 >= 80:
            cprint("\n  结论: 混合检索排序稳定性良好 ✓", Colors.GREEN)
        elif avg_5_20 >= 60:
            cprint("\n  结论: 混合检索排序有一定波动，中等稳定性 ⚠", Colors.YELLOW)
        else:
            cprint("\n  结论: 混合检索排序波动明显，可能存在噪声 ✗", Colors.RED)
            cprint("  建议: 考虑优化 RRF 融合参数或评估是否使用纯 dense 检索", Colors.YELLOW)
    
    # 最不稳定查询
    if all_stats:
        worst = sorted(all_stats, key=lambda s: s["consistency_5_20"])[:3]
        cprint("\n  最不稳定的 3 个查询:", Colors.YELLOW)
        for s in worst:
            q = s["query"][:30] + "..." if len(s["query"]) > 30 else s["query"]
            cprint(f"    {q:<30} 5 vs 20: {s['consistency_5_20']:.0f}%", Colors.YELLOW)
    
    # 返回统计列表供外部使用
    return all_stats


# ---------------------------------------------------------------------------
# Hybrid vs Dense-only 模拟分析
# ---------------------------------------------------------------------------
def analyze_hybrid_vs_dense(all_results: list[QueryResult]):
    """
    通过分数分布和结果多样性分析 hybrid 搜索的特征
    
    由于 API 不暴露 CandidateSource，我们通过以下指标间接分析:
    1. 分数集中度: Hybrid 通常分数更集中（RRF 缩放）
    2. 结果语言多样性: Dense-only 可能更集中
    3. Top-K 重叠率: 通过不同 limit 观察
    """
    print_section("Hybrid 搜索特征分析")
    
    cprint("  由于 API 不暴露 CandidateSource，我们通过以下指标间接分析:", Colors.DIM)
    cprint("  1. 分数集中度  2. 结果语言多样性  3. Top-K 重叠率\n", Colors.DIM)
    
    # 按类别分析
    category_idx = 0
    for category, queries in QUERY_SETS.items():
        category_idx += 1
        cprint(f"\n  ── [{category_idx}] {category} ──", Colors.BOLD)
        
        for query in queries:
            results = [qr for qr in all_results if qr.query == query and qr.limit == DEFAULT_LIMIT and qr.success]
            if not results:
                continue
            
            qr = results[0]
            scores = [r.score for r in qr.results]
            
            # 🛡️ 如果 scores 为空，跳过此查询
            if not scores:
                q = query[:30] + "..." if len(query) > 30 else query
                cprint(f"  \"{q}\" — (无分数数据，跳过)", Colors.DIM)
                continue
            
            langs = defaultdict(int)
            types = defaultdict(int)
            
            for r in qr.results:
                langs[r.language] += 1
                types[r.symbol_type] += 1
            
            # 分数变异系数 (标准差 / 均值)
            if scores:
                mean_score = sum(scores) / len(scores)
                variance = sum((s - mean_score) ** 2 for s in scores) / len(scores)
                std_score = variance ** 0.5
                cv = std_score / mean_score if mean_score > 0 else 0  # 变异系数
            else:
                cv = 0
            
            q = query[:30] + "..." if len(query) > 30 else query
            lang_diversity = len(langs)  # 语言多样性
            type_diversity = len(types)  # 类型多样性
            
            cprint(f"  \"{q}\"", Colors.BLUE)
            cprint(f"    结果数: {len(scores)} | 分数范围: {min(scores):.4f}-{max(scores):.4f} | CV: {cv:.4f}", Colors.DIM)
            cprint(f"    语言多样性: {lang_diversity} 种 | 类型多样性: {type_diversity} 种", Colors.DIM)
            
            if cv > 0.5:
                cprint("    → 分数离散度高，搜索结果相关性差异大", Colors.YELLOW)
            elif cv < 0.2:
                cprint("    → 分数集中度好，搜索结果相关性接近", Colors.GREEN)
            else:
                cprint("    → 分数中等离散", Colors.DIM)

# ---------------------------------------------------------------------------
# 结论与建议
# ---------------------------------------------------------------------------
def print_conclusions(all_results: list[QueryResult], stability_stats: list[dict]):
    """打印结论与建议"""
    print_section("结论与建议")
    
    successful = [qr for qr in all_results if qr.success and qr.results]
    failed = [qr for qr in all_results if not qr.success]
    
    # 总体评估
    cprint("  [1] 服务可用性", Colors.BOLD)
    if not failed:
        cprint("      所有查询均成功，服务运行正常 ✓", Colors.GREEN)
    else:
        cprint(f"      {len(failed)} 个查询失败，请检查服务状态", Colors.RED)
    
    # 性能评估
    cprint("\n  [2] 响应性能", Colors.BOLD)
    if successful:
        avg_time = sum(qr.elapsed for qr in successful) / len(successful)
        max_time = max(qr.elapsed for qr in successful)
        cprint(f"      平均响应时间: {avg_time:.2f}s", Colors.CYAN)
        cprint(f"      最大响应时间: {max_time:.2f}s", Colors.CYAN)
        if avg_time < 1.0:
            cprint("      性能评价: 优秀 ✓", Colors.GREEN)
        elif avg_time < 3.0:
            cprint("      性能评价: 良好 ✓", Colors.GREEN)
        else:
            cprint("      性能评价: 较慢 ⚠", Colors.YELLOW)
    
    # 混合检索评估
    cprint("\n  [3] 混合检索 (Hybrid) 评估", Colors.BOLD)
    if stability_stats:
        avg_5_20 = sum(s["consistency_5_20"] for s in stability_stats) / len(stability_stats)
        if avg_5_20 >= 80:
            cprint("      RRF 融合稳定性: 良好", Colors.GREEN)
            cprint("      当前配置下，混合检索表现稳定", Colors.GREEN)
        elif avg_5_20 >= 60:
            cprint("      RRF 融合稳定性: 中等", Colors.YELLOW)
            cprint("      混合检索排序有一定波动，建议监控", Colors.YELLOW)
        else:
            cprint("      RRF 融合稳定性: 较差", Colors.RED)
            cprint("      BM25 噪声可能影响了搜索结果质量", Colors.RED)
            cprint("      建议: 尝试纯 Dense 检索作为对比", Colors.YELLOW)
    
    # 建议
    cprint("\n  [4] 建议", Colors.BOLD)
    if stability_stats:
        avg_5_20 = sum(s["consistency_5_20"] for s in stability_stats) / len(stability_stats)
        
        if avg_5_20 < 70:
            cprint("      1. 考虑在 server.rs 中提供 dense-only 端点", Colors.YELLOW)
            cprint("         (设置 HybridSearchConfig.enable_sparse = false)", Colors.YELLOW)
            cprint("      2. 调整 RRF 融合参数 (rrf_k)", Colors.YELLOW)
            cprint("         更高的 rrf_k 会降低排名差异的影响", Colors.YELLOW)
            cprint("      3. 优化 BM25 索引质量", Colors.YELLOW)
            cprint("         检查分词器和索引配置", Colors.YELLOW)
        else:
            cprint("      1. 当前混合检索表现良好，可继续使用", Colors.GREEN)
            cprint("      2. 定期监控搜索结果质量", Colors.DIM)
            cprint("      3. 如有需要，可增加 Reranking 步骤", Colors.DIM)
    else:
        cprint("      1. 检查服务是否正常运行", Colors.YELLOW)
        cprint("      2. 确认 codebase 索引已创建完成", Colors.YELLOW)


# ---------------------------------------------------------------------------
# Hybrid 详情对比分析
# ---------------------------------------------------------------------------
def compare_hybrid_detail(all_results: list[QueryResult]):
    """
    对每个查询打印结果 symbol_name 列表，标记精确匹配
    
    策略: 对于精确符号查询（如 "semantic_search"），如果结果中的 symbol_name 
    包含查询关键词但排名靠后（6-10位），说明混合检索的排序有问题。
    """
    print_section("对比分析: Hybrid 详细结果分析")
    
    cprint("  ★ 标记表示 symbol_name 包含查询关键词", Colors.DIM)
    cprint("  如果 ★ 结果不在 Top-3，说明混合检索排序可能有问题\n", Colors.DIM)
    
    for category, queries in QUERY_SETS.items():
        cprint(f"\n  ── {category} ──", Colors.BOLD)
        
        for query in queries:
            # 获取 limit=10 的结果（如果有的话）
            results = [qr for qr in all_results if qr.query == query and qr.limit == DEFAULT_LIMIT and qr.success]
            if not results:
                # 尝试 limit=20
                results = [qr for qr in all_results if qr.query == query and qr.limit == 20 and qr.success]
            if not results:
                continue
            
            qr = results[0]
            if not qr.results:
                cprint(f"  查询: \"{query}\" — (无结果)", Colors.DIM)
                continue
            
            # 检查精确匹配
            query_lower = query.lower()
            
            # 打印 Top-10 或全部结果
            display_results = qr.results[:10] if len(qr.results) > 10 else qr.results
            
            header = f"\n  查询: \"{query}\" (limit={qr.limit}, 结果数={len(qr.results)})"
            cprint(header, Colors.BLUE)
            print_separator("─", 70)
            cprint(f"  {'#':<3} │ {'符号名':<35} │ {'分数':>7} │ {'匹配':>4}", Colors.CYAN)
            print("  " + "─" * 60)
            
            for r in display_results:
                is_exact_match = query_lower in r.symbol_name.lower()
                marker = "★" if is_exact_match else " "
                
                symbol_display = r.symbol_name[:35]
                if len(r.symbol_name) > 35:
                    symbol_display = symbol_display + "..."
                
                if is_exact_match and r.rank <= 3:
                    cprint(f"  {r.rank:<3} │ {symbol_display:<35} │ {r.score:>7.4f} │ {marker:>4} (Top-3 ✓)", Colors.GREEN)
                elif is_exact_match:
                    cprint(f"  {r.rank:<3} │ {symbol_display:<35} │ {r.score:>7.4f} │ {marker:>4} (排名靠后 ⚠)", Colors.YELLOW)
                else:
                    cprint(f"  {r.rank:<3} │ {symbol_display:<35} │ {r.score:>7.4f} │ {marker:>4}", Colors.DIM)
            
            # 统计精确匹配结果
            exact_matches = [(r.rank, r.symbol_name) for r in qr.results if query_lower in r.symbol_name.lower()]
            if exact_matches:
                ranks = [m[0] for m in exact_matches]
                cprint(f"\n  精确匹配结果位置: {ranks}", Colors.CYAN)
                if min(ranks) <= 3:
                    cprint(f"  → 精确匹配在 Top-3，排序良好 ✓", Colors.GREEN)
                else:
                    cprint(f"  → ⚠ 精确匹配在位置 {min(ranks)}，可能被关键词噪声淹没", Colors.YELLOW)
            else:
                cprint(f"\n  无精确匹配结果 (symbol_name 不包含查询关键词)", Colors.DIM)


# ---------------------------------------------------------------------------
# 分数分布可视化分析
# ---------------------------------------------------------------------------
def score_distribution_analysis(all_results: list[QueryResult]):
    """
    对 hybrid 结果的分数分布进行深入分析
    
    计算:
    - 百分位数 (P25, P50, P75, P90, P95, P99)
    - 相邻结果之间的分数差异
    - Top-1 和 Top-2 分数差异（如果 < 0.01，说明 RRF 融合导致区分度不足）
    """
    print_section("分数分布可视化分析")
    
    # 收集所有查询的分数
    all_scores: list[float] = []
    per_query_scores: dict[str, list[float]] = {}
    
    for query in set(qr.query for qr in all_results):
        results = [qr for qr in all_results if qr.query == query and qr.success and qr.results]
        if not results:
            continue
        scores = [r.score for r in results[0].results]
        per_query_scores[query] = scores
        all_scores.extend(scores)
    
    if not all_scores:
        cprint("  无分数数据可分析", Colors.YELLOW)
        return
    
    # 全局统计
    all_scores_sorted = sorted(all_scores)
    n = len(all_scores_sorted)
    
    cprint(f"\n  全局分数统计 (共 {n} 个结果)", Colors.BOLD)
    cprint(f"    最小值: {all_scores_sorted[0]:.4f}", Colors.DIM)
    cprint(f"    最大值: {all_scores_sorted[-1]:.4f}", Colors.DIM)
    cprint(f"    平均值: {sum(all_scores)/n:.4f}", Colors.DIM)
    
    # 百分位数
    def percentile(data, p):
        k = (len(data) - 1) * (p / 100)
        f = int(k)
        c = f + 1 if f + 1 < len(data) else f
        d = k - f
        return data[f] + d * (data[c] - data[f])
    
    p25 = percentile(all_scores_sorted, 25)
    p50 = percentile(all_scores_sorted, 50)
    p75 = percentile(all_scores_sorted, 75)
    p90 = percentile(all_scores_sorted, 90)
    p95 = percentile(all_scores_sorted, 95)
    p99 = percentile(all_scores_sorted, 99)
    
    cprint(f"\n  百分位数:", Colors.BOLD + Colors.CYAN)
    cprint(f"    P25: {p25:.4f}  |  P50 (中位数): {p50:.4f}  |  P75: {p75:.4f}", Colors.CYAN)
    cprint(f"    P90: {p90:.4f}  |  P95: {p95:.4f}  |  P99: {p99:.4f}", Colors.CYAN)
    
    # 分数区间分布
    bins = [(0, 0.1), (0.1, 0.2), (0.2, 0.3), (0.3, 0.4), (0.4, 0.5), (0.5, float('inf'))]
    cprint(f"\n  分数区间分布:", Colors.BOLD + Colors.CYAN)
    for low, high in bins:
        count = sum(1 for s in all_scores if low <= s < high)
        bar = "█" * min(count, 30)
        high_label = f"{high:.1f}" if high != float('inf') else "+∞"
        color = Colors.GREEN if low >= 0.4 else Colors.CYAN if low >= 0.2 else Colors.YELLOW if low >= 0.1 else Colors.DIM
        cprint(f"    [{low:.1f} - {high_label}): {count:>3} {bar}", color)
    
    # 每查询的分数下降分析
    cprint(f"\n  各查询分数下降分析 (Top-5 内部):", Colors.BOLD + Colors.CYAN)
    
    low_separation_count = 0
    total_queries = 0
    
    for query, scores in per_query_scores.items():
        if len(scores) < 2:
            continue
        total_queries += 1
        
        q = query[:30] + "..." if len(query) > 30 else query
        top5 = scores[:5]
        
        # 计算相邻分数差异
        gaps = [top5[i] - top5[i+1] for i in range(len(top5)-1)]
        avg_gap = sum(gaps) / len(gaps) if gaps else 0
        max_gap = max(gaps) if gaps else 0
        min_gap = min(gaps) if gaps else 0
        
        # Top-1 和 Top-2 的分数差异
        gap_1_2 = top5[0] - top5[1] if len(top5) >= 2 else 0
        
        gap_status = ""
        gap_color = Colors.GREEN
        if gap_1_2 < 0.01 and len(top5) >= 2:
            gap_status = "⚠ 区分度不足"
            gap_color = Colors.YELLOW
            low_separation_count += 1
        elif avg_gap < 0.01:
            gap_status = "⚠ 分数趋同"
            gap_color = Colors.YELLOW
        else:
            gap_status = "✓"
        
        cprint(f"  \"{q}\":", Colors.BLUE)
        cprint(f"    Top-5 分数: {[f'{s:.4f}' for s in top5]}", Colors.DIM)
        cprint(f"    相邻差异: [{', '.join(f'{g:.4f}' for g in gaps)}] (平均 {avg_gap:.4f}, 最大 {max_gap:.4f}, 最小 {min_gap:.4f})", Colors.DIM)
        cprint(f"    Top-1 vs Top-2 差异: {gap_1_2:.4f} {gap_status}", gap_color)
    
    # 总结
    print()
    print_separator("─", 70)
    cprint("  RRF 融合区分度分析:", Colors.BOLD + Colors.CYAN)
    if total_queries > 0:
        ratio = low_separation_count / total_queries * 100
        cprint(f"    区分度不足的查询: {low_separation_count}/{total_queries} ({ratio:.1f}%)", 
               Colors.YELLOW if ratio > 30 else Colors.GREEN)
        if ratio > 50:
            cprint("    → 警告: 超过 50% 的查询 Top-1 和 Top-2 分数差异很小", Colors.RED)
            cprint("    → 可能原因: RRF 融合使排名权重过于接近", Colors.YELLOW)
        elif ratio > 20:
            cprint("    → 注意: 部分查询存在区分度不足问题", Colors.YELLOW)
        else:
            cprint("    → RRF 融合区分度良好 ✓", Colors.GREEN)


# ---------------------------------------------------------------------------
# MRR (Mean Reciprocal Rank) 计算
# ---------------------------------------------------------------------------
def calculate_mrr(all_results: list[QueryResult], judgments: dict) -> float:
    """
    计算 MRR (Mean Reciprocal Rank) 指标
    
    基于 RELEVANCE_JUDGMENTS 定义的期望结果，自动计算每个查询的
     reciprocal rank (1/rank if relevant result found, 0 otherwise)，
    然后求平均。
    
    Args:
        all_results: 所有搜索结果
        judgments: {query: {"expected_symbols": [...]}}
    
    Returns:
        float: MRR 值 (0-1)，越高越好
    """
    if not judgments:
        return 0.0
    
    reciprocal_ranks: list[float] = []
    
    for query, judgment in judgments.items():
        expected = judgment.get("expected_symbols", [])
        if not expected:
            continue
        
        expected_set = set(expected)
        
        # 查找该查询的结果
        results = [qr for qr in all_results if qr.query == query and qr.success and qr.results]
        if not results:
            continue
        
        qr = results[0]
        
        # 查找第一个命中
        for rank, r in enumerate(qr.results, start=1):
            if r.symbol_name in expected_set:
                reciprocal_ranks.append(1.0 / rank)
                break
        else:
            reciprocal_ranks.append(0.0)
    
    if not reciprocal_ranks:
        return 0.0
    
    return sum(reciprocal_ranks) / len(reciprocal_ranks)


# ---------------------------------------------------------------------------
# 问题查询自动检测
# ---------------------------------------------------------------------------
def detect_problematic_queries(
    all_results: list[QueryResult],
    stability_stats: list[dict],
) -> list[dict]:
    """
    自动检测并列出"有问题的查询"
    
    检测规则:
    1. 返回结果数少于 limit 的 50%
    2. 结果数超过 0 但分数全部低于 0.1
    3. 与其他查询相比，响应时间异常（> 平均值的 2 倍）
    4. 稳定性分析中 5 vs 20 一致性低于 60%
    
    Returns:
        list[dict]: 问题查询列表，每个包含 query, limit, reasons
    """
    print_section("问题查询自动检测")
    
    problematic: list[dict] = []
    
    # 规则 1 & 2: 结果数量/分数问题
    cprint("\n  [规则 1-2] 结果数量与分数检测", Colors.BOLD)
    
    for qr in all_results:
        if not qr.success or not qr.results:
            continue
        
        reasons: list[str] = []
        
        # 规则 1: 结果数少于 limit 的 50%
        if len(qr.results) < qr.limit * 0.5:
            reasons.append(f"结果数过少 ({len(qr.results)}/{qr.limit}, 仅 {len(qr.results)/qr.limit*100:.0f}%)")
        
        # 规则 2: 分数全部低于 0.1
        scores = [r.score for r in qr.results]
        if scores and all(s < 0.1 for s in scores):
            reasons.append(f"分数过低 (max={max(scores):.4f} < 0.1)")
        
        if reasons:
            problematic.append({
                "query": qr.query,
                "limit": qr.limit,
                "reasons": reasons,
            })
    
    # 规则 3: 响应时间异常
    cprint("\n  [规则 3] 响应时间异常检测", Colors.BOLD)
    
    successful = [qr for qr in all_results if qr.success]
    if successful:
        avg_time = sum(qr.elapsed for qr in successful) / len(successful)
        time_threshold = avg_time * 2
        
        slow_queries = [qr for qr in successful if qr.elapsed > time_threshold]
        if slow_queries:
            cprint(f"    平均响应时间: {avg_time:.2f}s | 异常阈值 (>2x): {time_threshold:.2f}s", Colors.CYAN)
            for qr in slow_queries[:5]:
                q = qr.query[:30] + "..." if len(qr.query) > 30 else qr.query
                cprint(f"    ⚠ \"{q}\" — {qr.elapsed:.2f}s ({qr.elapsed/avg_time:.1f}x 平均)", Colors.YELLOW)
                problematic.append({
                    "query": qr.query,
                    "limit": qr.limit,
                    "reasons": [f"响应时间异常 ({qr.elapsed:.2f}s > {time_threshold:.2f}s)"],
                })
        else:
            cprint(f"    平均响应时间: {avg_time:.2f}s — 无异常 ✓", Colors.GREEN)
    
    # 规则 4: 稳定性问题
    cprint("\n  [规则 4] 稳定性异常检测 (5 vs 20 一致性 < 60%)", Colors.BOLD)
    
    if stability_stats:
        unstable = [s for s in stability_stats if s.get("consistency_5_20", 100) < 60]
        if unstable:
            for s in unstable[:5]:
                q = s.get("query", "unknown")[:30]
                c = s.get("consistency_5_20", 0)
                cprint(f"    ⚠ \"{q}\" — 5 vs 20 一致性: {c:.0f}%", Colors.YELLOW)
                problematic.append({
                    "query": s["query"],
                    "limit": "N/A (稳定性分析)",
                    "reasons": [f"稳定性不足 (5 vs 20: {c:.0f}% < 60%)"],
                })
        else:
            cprint(f"    所有查询稳定性良好 ✓", Colors.GREEN)
    
    # 汇总
    print()
    print_separator("═", 70)
    if problematic:
        # 去重
        seen = set()
        unique_problematic = []
        for p in problematic:
            key = (p["query"], tuple(sorted(p["reasons"])))
            if key not in seen:
                seen.add(key)
                unique_problematic.append(p)
        
        cprint(f"\n  共检测到 {len(unique_problematic)} 个问题查询:", Colors.BOLD + Colors.YELLOW)
        for i, p in enumerate(unique_problematic, 1):
            q = p["query"][:35] + "..." if len(p["query"]) > 35 else p["query"]
            cprint(f"\n  [{i}] \"{q}\" (limit={p['limit']})", Colors.YELLOW)
            for reason in p["reasons"]:
                cprint(f"      → {reason}", Colors.RED)
    else:
        cprint("\n  ✓ 未检测到明显问题查询", Colors.GREEN)
    
    return problematic


# ---------------------------------------------------------------------------
# 主流程
# ---------------------------------------------------------------------------
def main():
    """主测试流程"""
    print()
    cprint("======================================================================", Colors.BOLD)
    cprint("  CodeSeek Semantic Search 功能测试报告", Colors.BOLD + Colors.CYAN)
    cprint("======================================================================", Colors.BOLD)
    cprint(f"\n  服务地址: {BASE_URL}", Colors.DIM)
    cprint(f"  测试时间: {time.strftime('%Y-%m-%d %H:%M:%S')}\n", Colors.DIM)
    
    # ------------------------------------------------------------------
    # Step 1: 健康检查
    # ------------------------------------------------------------------
    print_section("[1/5] 健康检查")
    try:
        health = check_health()
        if all(v.get("status") == "OK" for v in health.values()):
            cprint("\n  服务正常运行，可以继续测试 ✓", Colors.GREEN)
        else:
            cprint("\n  ⚠ 部分端点不可用，测试结果可能不准确", Colors.YELLOW)
    except Exception as e:
        cprint(f"\n  ✗ 健康检查失败: {e}", Colors.RED)
        cprint("  请确认服务已在 127.0.0.1:12800 运行", Colors.YELLOW)
        sys.exit(1)
    
    # ------------------------------------------------------------------
    # Step 2: 语义搜索测试（所有查询）
    # ------------------------------------------------------------------
    print_section("[2/5] 语义搜索测试")
    
    all_results: list[QueryResult] = []
    
    for category, queries in QUERY_SETS.items():
        cprint(f"\n  ── {category} ──", Colors.BOLD)
        
        for query in queries:
            qr = semantic_search(query, DEFAULT_LIMIT)
            all_results.append(qr)
            print_query_report(qr)
    
    # ------------------------------------------------------------------
    # Step 3: 全局统计
    # ------------------------------------------------------------------
    print_global_statistics(all_results)
    
    # ------------------------------------------------------------------
    # Step 4: 混合检索稳定性分析（不同 limit）
    # ------------------------------------------------------------------
    print_section("[3/5] Hybrid 稳定性分析")
    cprint("  正在对每个查询执行 limit=5, 10, 20 的搜索...", Colors.DIM)
    
    # 并发执行稳定性分析
    all_results_stability: dict[str, dict[int, QueryResult]] = defaultdict(dict)
    
    tasks = []
    for query in QUERY_SETS.get("概念搜索（语义理解）", []) + QUERY_SETS.get("精确符号搜索（关键词匹配）", []):
        for limit in [5, 10, 20]:
            tasks.append((query, limit))
    
    # 限制并发数
    with ThreadPoolExecutor(max_workers=6) as executor:
        futures = {executor.submit(semantic_search, q, l): (q, l) for q, l in tasks}
        for future in as_completed(futures):
            q, l = futures[future]
            try:
                result = future.result()
                all_results_stability[q][l] = result
                status = f"✓" if result.success else f"✗"
                cprint(f"    {status} {q[:25]:<25} limit={l:2d} ({result.elapsed:.2f}s)", Colors.DIM)
            except Exception as e:
                all_results_stability[q][l] = QueryResult(
                    query=q, limit=l, elapsed=0, results=[],
                    success=False, error=str(e)
                )
                cprint(f"    ✗ {q[:25]:<25} limit={l:2d} ERROR", Colors.RED)
    
    # 合并稳定性结果到所有结果中
    all_results.extend([
        r for results in all_results_stability.values()
        for r in results.values()
    ])
    
    # 执行稳定性分析（捕获返回值）
    stability_stats = stability_analysis(all_results_stability)
    
    # ------------------------------------------------------------------
    # Step 5: Hybrid 详情对比分析 (增强)
    # ------------------------------------------------------------------
    print_section("[5/8] Hybrid 详情对比分析")
    compare_hybrid_detail(all_results)
    
    # ------------------------------------------------------------------
    # Step 6: Hybrid vs Dense 模拟分析
    # ------------------------------------------------------------------
    print_section("[6/8] Hybrid 搜索特征分析")
    analyze_hybrid_vs_dense(all_results)
    
    # ------------------------------------------------------------------
    # Step 7: 分数分布分析 (增强)
    # ------------------------------------------------------------------
    print_section("[7/8] 分数分布与 MRR 评估")
    score_distribution_analysis(all_results)
    
    # 计算 MRR（如果定义了 relevance judgments）
    if RELEVANCE_JUDGMENTS:
        mrr = calculate_mrr(all_results, RELEVANCE_JUDGMENTS)
        print()
        cprint(f"  MRR (Mean Reciprocal Rank): {mrr:.4f}", Colors.BOLD + Colors.CYAN)
        if mrr >= 0.8:
            cprint("  → 检索质量优秀 ✓", Colors.GREEN)
        elif mrr >= 0.5:
            cprint("  → 检索质量良好 ✓", Colors.GREEN)
        elif mrr > 0:
            cprint("  → 检索质量一般 ⚠", Colors.YELLOW)
        else:
            cprint("  → 未找到标注结果，请检查 RELEVANCE_JUDGMENTS", Colors.YELLOW)
    else:
        cprint("\n  RELEVANCE_JUDGMENTS 为空，跳过 MRR 计算", Colors.DIM)
        cprint("  (请在文件顶部的 RELEVANCE_JUDGMENTS 中定义期望结果)", Colors.DIM)
    
    # ------------------------------------------------------------------
    # Step 8: 问题检测 & 结论与建议
    # ------------------------------------------------------------------
    print_section("[8/8] 结论与建议")
    
    # 问题查询检测
    problematic = detect_problematic_queries(all_results, stability_stats)
    
    # 打印结论
    print_conclusions(all_results, stability_stats)
    
    # 问题查询汇总
    if problematic:
        print()
        cprint("  ⚠ 检测到的问题查询汇总:", Colors.BOLD + Colors.YELLOW)
        seen_queries = set()
        for p in problematic:
            key = p["query"]
            if key not in seen_queries:
                seen_queries.add(key)
                q = key[:40] + "..." if len(key) > 40 else key
                reasons_str = "; ".join(p["reasons"])
                cprint(f"    - \"{q}\"", Colors.YELLOW)
                cprint(f"      原因: {reasons_str}", Colors.RED)
    
    # ------------------------------------------------------------------
    # 最终摘要
    # ------------------------------------------------------------------
    print()
    print_separator("═", 70)
    cprint("  测试完成！", Colors.BOLD + Colors.GREEN)
    print_separator("═", 70)
    
    total = len(all_results)
    success_count = sum(1 for qr in all_results if qr.success)
    fail_count = total - success_count
    
    cprint(f"\n  总请求数: {total}", Colors.BOLD)
    cprint(f"  成功: {success_count}", Colors.GREEN)
    if fail_count:
        cprint(f"  失败: {fail_count}", Colors.RED)
    
    print()

if __name__ == "__main__":
    main()
