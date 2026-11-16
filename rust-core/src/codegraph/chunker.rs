//! Chunk extraction from Tree-sitter AST.
//!
//! This module converts AST symbol instances (functions, structs, etc.)
//! into `CodeChunk` objects that are used by both the dense vector index
//! and the sparse BM25 index.
//!
//! 除了「把符号变成 chunk」，本模块还承担**唯一一份**的范围选择逻辑
//! （[`select_effective_ranges`]）：索引口径（哪些符号进索引）在这里判定一次，索引构建层
//! （`services::embedding_service::process_file_content`）必须复用它，禁止另写副本。
//!
//! **内容降级（容器收缩成签名骨架）已停用** —— 开关见 [`SHRINK_CONTAINER_CONTENT`]：
//! 所有候选的 `content` 与 `line_start` / `line_end` 一律取自 `full_range`，
//! 父子包含关系改由**排序期**做无损降权
//! （`services::hybrid_search::demote_enclosing_candidates`）。

use crate::storage::traits_bm25::CodeChunk;
use crate::codegraph::treesitter::ast_instance_structs::AstSymbolInstanceArc;
use crate::codegraph::treesitter::structs::SymbolType;
use std::path::Path;
use tree_sitter::Range;

/// 是否属于「要进索引」的符号类型。
///
/// 这是全仓**唯一一份**类型口径：`extract_chunks` 与
/// `services::embedding_service::process_file_content` 都经由
/// [`select_effective_ranges`] 复用该判定，不得再各自写一遍 `match symbol_type`。
fn is_indexable_symbol_type(symbol_type: SymbolType) -> bool {
    matches!(
        symbol_type,
        SymbolType::StructDeclaration | SymbolType::FunctionDeclaration
    )
}

/// 是否把容器 chunk 的内容收缩到 `declaration_range`（「内容降级」开关）。
///
/// **恒为 `false` —— 实测证明内容降级是负优化，不要重新打开。**
///
/// 降级版把父容器 chunk 的 content 从 `full_range` 换成 `declaration_range`
/// （只剩签名骨架），RepoQA 总体由 82.0% 掉到 78.0%：cpp 45%→55%、java 85%→90% 确有提升，
/// 但 **typescript 100%→70%、rust 90%→85%** 严重回退。
///
/// 回退题的 rank1 实测全部是 1~5 行短块（例：express `Route` 的 rank1 `line_span=[43,43]`）：
/// 容器降级后变成「超短文档 + 符号名完美词面命中」，在 BM25 的长度归一化下反而抢到 rank1，
/// 正文却只剩签名 ⇒ BLEU 趋近 0。
///
/// 结论：**不能用删内容的方式解决排序问题**。内容必须保持 `full_range`；父子包含关系改为
/// 在**排序期**无损降权（`services::hybrid_search::demote_enclosing_candidates`）——
/// 降权可解释、可回滚，且不损失任何正文。
const SHRINK_CONTAINER_CONTENT: bool = false;

/// 一个索引候选符号经「父子包含判定」后的有效范围。
#[derive(Debug, Clone)]
pub struct EffectiveRange {
    /// parser 给出的原始完整范围（本层永不修改）
    pub full_range: Range,
    /// 实际用于取内容与行号的范围。
    ///
    /// 当前 [`SHRINK_CONTAINER_CONTENT`] 为 `false`，因此**恒等于 `full_range`**；
    /// 该字段保留是为了不破坏调用方契约，并给「日后若有新的无损用法」留一个唯一入口。
    pub effective_range: Range,
    /// 该符号的 `full_range` 是否严格包含同文件其它候选的 `full_range`。
    ///
    /// 仅供观测/日后排序期优化参考：**当前不影响内容范围**（容器不再收缩）。
    pub is_container: bool,
    /// 容器但 `declaration_range` 不可收缩（为空/越界/不比 full 小）而保留 `full_range`。
    ///
    /// 降级停用后恒为 `false`（不再尝试收缩，也就没有「收缩被拒」这回事）。
    pub shrink_rejected: bool,
}

impl EffectiveRange {
    /// 用有效范围从整份文件内容中切出 chunk 文本与 1-indexed 行区间。
    ///
    /// 返回 `(content, line_start, line_end)`，三者来自**同一个**范围；
    /// 若有效范围在该文件上切不出来（越界 / 非 char boundary），回退到 `full_range` —
    /// 宁可退回大块，也绝不丢 chunk。
    pub fn content_and_lines(&self, file_content: &str) -> Option<(String, usize, usize)> {
        content_and_lines(file_content, &self.effective_range)
            .or_else(|| content_and_lines(file_content, &self.full_range))
    }
}

/// 按 `range` 取整行文本及其 1-indexed 行区间。
///
/// 语义与 `SymbolInformation::get_content` 一致（按行切片、保留行首缩进），
/// 区别只在于范围由调用方显式指定 —— 于是 content 与行号永远切自**同一个**范围，
/// 不会出现「content 是签名、行号却还是整块」的错位（当前 [`SHRINK_CONTAINER_CONTENT`]
/// 关闭，容器也走 `full_range`）。
///
/// 返回的 `line_end` 与 `content` 严格自洽：`content.lines().count() == line_end - line_start + 1`。
fn content_and_lines(file_content: &str, range: &Range) -> Option<(String, usize, usize)> {
    // 字节范围必须落在文件内且位于 char boundary，否则 `get` 返回 None
    let raw = file_content.get(range.start_byte..range.end_byte)?;
    let lines: Vec<&str> = file_content.split('\n').collect();

    let start_row = range.start_point.row.min(lines.len());
    let mut end_row = range.end_point.row.saturating_add(1); // exclusive, 0-indexed
    if raw.ends_with('\n') && end_row > start_row + 1 {
        // 末尾换行不额外占一行（与 get_content 的 `end_row -= 1` 同语义）
        end_row -= 1;
    }
    let end_row = end_row.min(lines.len());
    if start_row >= end_row {
        return None;
    }

    // end_row 是 0-indexed 的 exclusive 上界，数值上恰好等于 1-indexed 的末尾行号
    Some((lines[start_row..end_row].join("\n"), start_row + 1, end_row))
}

/// 选出每个候选符号的有效内容范围（索引口径 + 范围判定的**唯一实现**）。
///
/// 规则：
/// 1. 候选集 = `StructDeclaration | FunctionDeclaration`（见 [`is_indexable_symbol_type`]）；
/// 2. 若候选 A 的 `full_range` **严格包含**另一候选 B 的 `full_range`
///    （`B.start >= A.start && B.end <= A.end`，且至少一端严格不等），A 记为容器
///    —— 该标记仅作元数据，**不再触发任何内容收缩**；
/// 3. **所有候选的有效范围 = `full_range`**：内容与行号必须与降级前逐字节一致，
///    父子包含关系交给排序期无损降权（理由见 [`SHRINK_CONTAINER_CONTENT`]）；
/// 4. [`SHRINK_CONTAINER_CONTENT`] 是唯一保留的收缩开关，恒为 `false`。
///
/// 判定基于**同一文件的全部候选**（不是只看相邻项）。返回值下标与 `symbols` 一一对应：
/// `None` = 非候选（类型不在索引口径内），`Some(_)` = 该符号应采用的有效范围。
pub fn select_effective_ranges(symbols: &[AstSymbolInstanceArc]) -> Vec<Option<EffectiveRange>> {
    // ── 1) 收集候选的原始范围（尽早释放读锁，避免 O(n²) 判定期间持有 guard）──
    struct Candidate {
        index: usize,
        full: Range,
        declaration: Range,
    }

    let mut candidates: Vec<Candidate> = Vec::new();
    for (index, symbol) in symbols.iter().enumerate() {
        let guard = symbol.read();
        let symbol_ref = guard.as_ref();
        if !is_indexable_symbol_type(symbol_ref.symbol_type()) {
            continue;
        }
        candidates.push(Candidate {
            index,
            full: symbol_ref.full_range().clone(),
            declaration: symbol_ref.declaration_range().clone(),
        });
    }

    // ── 2) 严格包含判定 + 3) 范围收缩 ──
    let mut result: Vec<Option<EffectiveRange>> = vec![None; symbols.len()];
    for (position, candidate) in candidates.iter().enumerate() {
        let is_container = candidates
            .iter()
            .enumerate()
            .any(|(other_position, other)| {
                other_position != position && strictly_contains(&candidate.full, &other.full)
            });

        // ── 内容范围：降级已停用（见 `SHRINK_CONTAINER_CONTENT`）──
        // 一律取 `full_range`：容器与成员都保留完整正文，content 与行号与 82% 基线逐字节一致。
        // `is_container` 仍然照旧判定，但只作为元数据（观测 / 排序期参考），不影响取内容。
        let (effective_range, shrink_rejected) = if is_container && SHRINK_CONTAINER_CONTENT {
            match shrink_to_declaration(&candidate.full, &candidate.declaration) {
                Some(range) => (range, false),
                None => (candidate.full.clone(), true),
            }
        } else {
            (candidate.full.clone(), false)
        };

        result[candidate.index] = Some(EffectiveRange {
            full_range: candidate.full.clone(),
            effective_range,
            is_container,
            shrink_rejected,
        });
    }

    result
}

/// `outer` 是否**严格包含** `inner`（按字节范围，至少一端严格不等）。
fn strictly_contains(outer: &Range, inner: &Range) -> bool {
    inner.start_byte >= outer.start_byte
        && inner.end_byte <= outer.end_byte
        && (inner.start_byte > outer.start_byte || inner.end_byte < outer.end_byte)
}

/// 尝试把容器收缩到 `declaration_range`；不可用或并不更小时返回 `None`（调用方保留 full_range）。
fn shrink_to_declaration(full: &Range, declaration: &Range) -> Option<Range> {
    // 空范围 / 倒置范围（无 name 的匿名符号、parser 未填充等）
    if declaration.end_byte <= declaration.start_byte {
        return None;
    }
    // 必须完全落在 full_range 内。full_range 来自 tree-sitter，本身必在文件内，
    // 因此这一条同时挡住「declaration_range 超出文件长度」的脏数据。
    if declaration.start_byte < full.start_byte || declaration.end_byte > full.end_byte {
        return None;
    }
    // 行点必须单调，否则按行取内容会错位
    if declaration.end_point.row < declaration.start_point.row {
        return None;
    }
    // 必须**严格更小**，否则收缩无意义
    let full_len = full.end_byte.saturating_sub(full.start_byte);
    if declaration.end_byte - declaration.start_byte >= full_len {
        return None;
    }
    Some(declaration.clone())
}

/// Extract code chunks from parsed AST symbols.
///
/// Each indexable AST symbol (FunctionDeclaration, StructDeclaration) becomes one
/// `CodeChunk`. 所有 chunk（含把成员符号整块吞进去的容器符号）的 content 与行号
/// 都取自 `full_range` —— 内容降级已停用，理由见 [`SHRINK_CONTAINER_CONTENT`]。
///
/// # Arguments
/// * `file_path` - The path to the source file
/// * `symbols` - Parsed AST symbol instances from Tree-sitter
/// * `language` - The programming language identifier
///
/// # Returns
/// A vector of CodeChunk objects, one per indexable AST symbol.
pub fn extract_chunks(
    file_path: &Path,
    symbols: &[AstSymbolInstanceArc],
    language: &str,
) -> Vec<CodeChunk> {
    // 一次读文件：剪枝后的 content 与 line_start/line_end 必须来自**同一个**范围。
    let file_content = match std::fs::read_to_string(file_path) {
        Ok(content) => content,
        Err(_) => return Vec::new(),
    };

    let effective_ranges = select_effective_ranges(symbols);
    let mut chunks = Vec::new();

    for (index, symbol) in symbols.iter().enumerate() {
        let effective = match effective_ranges.get(index).and_then(|entry| entry.as_ref()) {
            Some(effective) => effective,
            None => continue, // 非候选：类型不在索引口径内
        };
        let (code_block, line_start, line_end) = match effective.content_and_lines(&file_content) {
            Some(fields) if !fields.0.is_empty() => fields,
            _ => continue,
        };

        let guard = symbol.read();
        let symbol_ref = guard.as_ref();
        chunks.push(CodeChunk::new(
            file_path.to_string_lossy().into_owned(),
            code_block,
            symbol_ref.name().to_string(),
            format!("{:?}", symbol_ref.symbol_type()),
            language.to_string(),
            line_start,
            line_end,
        ));
    }

    chunks
}

/// Extract chunks from a raw source file using the provided symbols.
///
/// This is a convenience wrapper that delegates to `extract_chunks`.
pub fn extract_chunks_from_symbols(
    file_path: impl AsRef<Path>,
    symbols: &[AstSymbolInstanceArc],
    language: &str,
) -> Vec<CodeChunk> {
    extract_chunks(file_path.as_ref(), symbols, language)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Point;

    fn range(start_byte: usize, end_byte: usize, start_row: usize, end_row: usize) -> Range {
        Range {
            start_byte,
            end_byte,
            start_point: Point { row: start_row, column: 0 },
            end_point: Point { row: end_row, column: 0 },
        }
    }

    #[test]
    fn strict_containment_requires_strictly_inner_range() {
        let outer = range(0, 100, 0, 10);

        // 完全落在内部：容器
        assert!(strictly_contains(&outer, &range(10, 90, 1, 9)));
        // 仅一端严格不等也算严格包含
        assert!(strictly_contains(&outer, &range(0, 90, 0, 9)));
        assert!(strictly_contains(&outer, &range(10, 100, 1, 10)));
        // 完全相同 / 部分重叠都不算
        assert!(!strictly_contains(&outer, &range(0, 100, 0, 10)));
        assert!(!strictly_contains(&outer, &range(50, 150, 5, 15)));
        assert!(!strictly_contains(&outer, &range(90, 110, 9, 11)));
    }

    #[test]
    fn shrink_to_declaration_rejects_unusable_or_non_shrinking_ranges() {
        let full = range(100, 500, 10, 40);

        // 正常签名骨架：更短 → 收缩
        let declaration = range(100, 160, 10, 10);
        assert_eq!(
            shrink_to_declaration(&full, &declaration).map(|r| (r.start_byte, r.end_byte)),
            Some((100, 160))
        );

        // 匿名 / 无 name：declaration_range == full_range → 保留 full，禁止丢 chunk
        assert!(shrink_to_declaration(&full, &full).is_none());
        // end_byte <= start_byte（倒置/空）
        assert!(shrink_to_declaration(&full, &range(200, 150, 20, 19)).is_none());
        assert!(shrink_to_declaration(&full, &range(200, 200, 20, 20)).is_none());
        // 超出 full（等价于超出文件长度）
        assert!(shrink_to_declaration(&full, &range(100, 900, 10, 60)).is_none());
        // 起点早于 full 起点
        assert!(shrink_to_declaration(&full, &range(50, 160, 5, 10)).is_none());
        // 行点倒置
        assert!(shrink_to_declaration(&full, &range(100, 160, 20, 10)).is_none());
    }

    #[test]
    fn content_and_lines_are_self_consistent() {
        let content = "line1\nline2\nline3\nline4\n";
        let start = content.find("line2").unwrap();
        let end = content.find("line3").unwrap() + "line3".len();
        let selection = Range {
            start_byte: start,
            end_byte: end,
            start_point: Point { row: 1, column: 0 },
            end_point: Point { row: 2, column: 5 },
        };

        let (text, line_start, line_end) =
            content_and_lines(content, &selection).expect("应能切出 line2..line3");

        assert_eq!(text, "line2\nline3");
        assert_eq!((line_start, line_end), (2, 3));
        // 不变式：content 行数 == line_end - line_start + 1
        assert_eq!(text.lines().count(), line_end - line_start + 1);
    }

    #[test]
    fn empty_symbol_slice_yields_empty_selection() {
        assert!(select_effective_ranges(&[]).is_empty());
    }
}
