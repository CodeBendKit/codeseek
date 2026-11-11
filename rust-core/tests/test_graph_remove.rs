//! 回归测试：petgraph 节点删除（swap-remove）导致的索引映射腐蚀
//!
//! 背景：`Graph::remove_node(idx)` 会把当前**最后一个节点**搬移到 `idx`，而
//! `PetCodeGraph` / `EntityGraph` 用 HashMap 缓存 `NodeIndex`。删除后若不修正被
//! 搬移节点的映射，按 id 取到的 NodeIndex 会指向另一个函数，于是
//! `add_call_relation` / `get_callers` / `get_callees` 静默连错边。
//!
//! 本文件完全自包含：纯内存构造 `FunctionInfo` / `CallRelation`，不依赖
//! `tests/test_repos` 固定路径，只用标准 `assert!` 宏 + `println!`。

use std::collections::HashSet;
use std::path::PathBuf;

use uuid::Uuid;

use codeseek::codegraph::types::{
    CallRelation, ClassInfo, ClassType, EntityEdge, EntityEdgeType, EntityGraph, EntityNode,
    FunctionInfo, PetCodeGraph,
};

// ---------------------------------------------------------------------------
// 构造辅助
// ---------------------------------------------------------------------------

fn make_function(name: &str, file: &str, language: &str) -> FunctionInfo {
    FunctionInfo {
        id: Uuid::new_v4(),
        name: name.to_string(),
        file_path: PathBuf::from(file),
        line_start: 1,
        line_end: 20,
        namespace: "test::ns".to_string(),
        language: language.to_string(),
        signature: Some(format!("fn {}()", name)),
        module_path: None,
        qualified_name: None,
    }
}

fn make_call(caller: &FunctionInfo, callee: &FunctionInfo, line: usize) -> CallRelation {
    CallRelation {
        caller_id: caller.id,
        callee_id: callee.id,
        caller_name: caller.name.clone(),
        callee_name: callee.name.clone(),
        caller_file: caller.file_path.clone(),
        callee_file: callee.file_path.clone(),
        line_number: line,
        is_resolved: true,
        resolution_level: 0,
        confidence: 0,
        rank: 0,
        candidate_count: 0,
    }
}

fn make_class(name: &str, file: &str, language: &str) -> ClassInfo {
    ClassInfo {
        id: Uuid::new_v4(),
        name: name.to_string(),
        file_path: PathBuf::from(file),
        line_start: 1,
        line_end: 50,
        namespace: "test::ns".to_string(),
        language: language.to_string(),
        class_type: ClassType::Class,
        parent_class: None,
        implemented_interfaces: Vec::new(),
        member_functions: Vec::new(),
        member_variables: Vec::new(),
    }
}

fn entity_edge(source: Uuid, target: Uuid, edge_type: EntityEdgeType) -> EntityEdge {
    EntityEdge {
        source,
        target,
        edge_type,
        metadata: None,
    }
}

fn names_of(items: Vec<(&FunctionInfo, &CallRelation)>) -> HashSet<String> {
    items.into_iter().map(|(f, _)| f.name.clone()).collect()
}

fn expected_names(names: &[&str]) -> HashSet<String> {
    names.iter().map(|n| n.to_string()).collect()
}

// ---------------------------------------------------------------------------
// 不变量断言辅助
// ---------------------------------------------------------------------------

/// 校验 `PetCodeGraph` 的索引映射不变量：
/// 1. `function_to_node[&id]` 指向的节点权重满足 `node_weight(idx).id == id`
/// 2. `node_to_function` 与 `function_to_node` 互为逆映射，且 `node_count == function_to_node.len()`
/// 3. 每条边的端点与边权重里的 caller_id/callee_id 一致（原 bug 的静默失败点）
/// 4. `stats.total_functions == node_count()`；`resolved/unresolved_calls == 实际边计数`
fn assert_call_graph_invariants(graph: &PetCodeGraph, ctx: &str) {
    assert_eq!(
        graph.graph.node_count(),
        graph.function_to_node.len(),
        "[{}] 节点数应与 function_to_node 大小一致",
        ctx
    );

    for (id, idx) in &graph.function_to_node {
        let weight = graph.graph.node_weight(*idx).unwrap_or_else(|| {
            panic!("[{}] function_to_node 指向不存在的节点 {:?} (id {})", ctx, idx, id)
        });
        assert_eq!(
            &weight.id, id,
            "[{}] 映射腐蚀：节点 {:?} 上实际是 {}，但 function_to_node 说是 {}",
            ctx, idx, weight.id, id
        );
        assert_eq!(
            graph.node_to_function.get(idx),
            Some(id),
            "[{}] node_to_function 与 function_to_node 不一致（节点 {:?}）",
            ctx,
            idx
        );
    }

    for (idx, id) in &graph.node_to_function {
        assert!(
            graph.graph.node_weight(*idx).is_some(),
            "[{}] node_to_function 指向不存在节点 {:?}",
            ctx,
            idx
        );
        assert_eq!(
            graph.function_to_node.get(id),
            Some(idx),
            "[{}] node_to_function 有孤儿项：节点 {:?} -> {}",
            ctx,
            idx,
            id
        );
    }

    for edge_index in graph.graph.edge_indices() {
        let (src, dst) = graph
            .graph
            .edge_endpoints(edge_index)
            .expect("edge endpoints must exist");
        let relation = graph.graph.edge_weight(edge_index).expect("edge weight must exist");
        assert_eq!(
            graph.graph.node_weight(src).expect("src node").id,
            relation.caller_id,
            "[{}] 边的 source 端点错位",
            ctx
        );
        assert_eq!(
            graph.graph.node_weight(dst).expect("dst node").id,
            relation.callee_id,
            "[{}] 边的 target 端点错位",
            ctx
        );
    }

    assert_eq!(
        graph.stats.total_functions,
        graph.graph.node_count(),
        "[{}] stats.total_functions 应等于剩余节点数",
        ctx
    );

    let mut resolved = 0usize;
    let mut unresolved = 0usize;
    for relation in graph.graph.edge_weights() {
        if relation.is_resolved {
            resolved += 1;
        } else {
            unresolved += 1;
        }
    }
    assert_eq!(
        graph.stats.resolved_calls, resolved,
        "[{}] stats.resolved_calls 应等于图中 resolved 边数",
        ctx
    );
    assert_eq!(
        graph.stats.unresolved_calls, unresolved,
        "[{}] stats.unresolved_calls 应等于图中 unresolved 边数",
        ctx
    );
}

/// 校验 `EntityGraph` 的同类不变量（含边端点归属）
fn assert_entity_graph_invariants(graph: &EntityGraph, ctx: &str) {
    assert_eq!(
        graph.graph.node_count(),
        graph.entity_to_node.len(),
        "[{}] 节点数应与 entity_to_node 大小一致",
        ctx
    );

    let id_of = |node: &EntityNode| match node {
        EntityNode::Class(class) => class.id,
        EntityNode::Function(function) => function.id,
    };

    for (id, idx) in &graph.entity_to_node {
        let node = graph.graph.node_weight(*idx).unwrap_or_else(|| {
            panic!("[{}] entity_to_node 指向不存在的节点 {:?} (id {})", ctx, idx, id)
        });
        assert_eq!(
            &id_of(node),
            id,
            "[{}] 映射腐蚀：节点 {:?} 上是 {}，但 entity_to_node 说是 {}",
            ctx,
            idx,
            id_of(node),
            id
        );
        assert_eq!(
            graph.node_to_entity.get(idx),
            Some(id),
            "[{}] node_to_entity 与 entity_to_node 不一致（节点 {:?}）",
            ctx,
            idx
        );
    }

    for (idx, id) in &graph.node_to_entity {
        assert!(
            graph.graph.node_weight(*idx).is_some(),
            "[{}] node_to_entity 指向不存在节点 {:?}",
            ctx,
            idx
        );
        assert_eq!(
            graph.entity_to_node.get(id),
            Some(idx),
            "[{}] node_to_entity 有孤儿项：节点 {:?} -> {}",
            ctx,
            idx,
            id
        );
    }

    for edge in graph.graph.edge_weights() {
        let src_idx = graph
            .entity_to_node
            .get(&edge.source)
            .unwrap_or_else(|| panic!("[{}] 边 source {} 已不在映射中", ctx, edge.source));
        let dst_idx = graph
            .entity_to_node
            .get(&edge.target)
            .unwrap_or_else(|| panic!("[{}] 边 target {} 已不在映射中", ctx, edge.target));
        let src_node = graph.graph.node_weight(*src_idx).expect("src node exists");
        let dst_node = graph.graph.node_weight(*dst_idx).expect("dst node exists");
        assert_eq!(
            id_of(src_node),
            edge.source,
            "[{}] 边 source 端点映射错位",
            ctx
        );
        assert_eq!(
            id_of(dst_node),
            edge.target,
            "[{}] 边 target 端点映射错位",
            ctx
        );
    }
}

// ---------------------------------------------------------------------------
// 测试 1：删除非末尾节点后映射与调用关系仍正确
// ---------------------------------------------------------------------------

#[test]
fn test_remove_middle_node_preserves_mapping_and_calls() {
    let file_a = "src/a.rs";
    let file_b = "src/b.rs";

    let f1 = make_function("alpha", file_a, "Rust");
    let f2 = make_function("beta", file_a, "Rust");
    let f3 = make_function("gamma", file_a, "Rust");
    let f4 = make_function("delta", file_b, "Rust");
    let f5 = make_function("epsilon", file_b, "Python");

    let mut graph = PetCodeGraph::new();
    for f in [&f1, &f2, &f3, &f4, &f5] {
        graph.add_function(f.clone());
    }
    for (caller, callee) in [(&f1, &f2), (&f2, &f3), (&f3, &f4), (&f5, &f1), (&f1, &f5)] {
        graph
            .add_call_relation(make_call(caller, callee, 10))
            .expect("add_call_relation should succeed");
    }
    graph.update_stats();
    assert_call_graph_invariants(&graph, "初始状态");
    println!(
        "初始: {} 节点 / {} 边 / resolved_calls={}",
        graph.graph.node_count(),
        graph.graph.edge_count(),
        graph.stats.resolved_calls
    );

    // 删除前的调用关系快照
    assert_eq!(
        names_of(graph.get_callees(&f1.id)),
        expected_names(&["beta", "epsilon"]),
        "删除前 f1 的被调用者"
    );
    assert_eq!(
        names_of(graph.get_callers(&f4.id)),
        expected_names(&["gamma"]),
        "删除前 f4 的调用者"
    );

    // f2 位于 index 1，last index = 4 → 必然触发 swap 搬移
    assert!(graph.remove_function(&f2.id), "删除已存在节点应返回 true");
    assert!(!graph.remove_function(&f2.id), "重复删除同一节点应返回 false");
    assert!(
        !graph.remove_function(&Uuid::new_v4()),
        "删除不存在的节点应返回 false"
    );

    assert_call_graph_invariants(&graph, "删除 f2 之后");
    assert!(
        graph.get_function_by_id(&f2.id).is_none(),
        "被删节点不应还能按 id 取到"
    );
    assert!(
        graph.find_functions_by_name("beta").is_empty(),
        "function_names 应清掉被删 id"
    );
    // f1->f2、f2->f3 级联消失，resolved 由 5 降到 3，不应虚高
    assert_eq!(graph.graph.edge_count(), 3, "两条关联边应随节点级联删除");
    assert_eq!(graph.stats.resolved_calls, 3, "删除后 resolved_calls 不应虚高");

    // 被搬移的 f5（原 last 节点）映射必须仍指向自己
    assert_eq!(
        graph
            .get_function_by_id(&f5.id)
            .expect("f5 应仍在图中")
            .name,
        "epsilon",
        "swap 搬移后按 id 取回的函数名不应错位"
    );
    assert_eq!(
        names_of(graph.get_callees(&f1.id)),
        expected_names(&["epsilon"]),
        "删除 f2 后 f1 的被调用者"
    );
    assert_eq!(
        names_of(graph.get_callers(&f4.id)),
        expected_names(&["gamma"]),
        "删除 f2 不应影响 f4 的调用者"
    );
    assert_eq!(
        graph
            .find_functions_by_file(&PathBuf::from(file_a))
            .len(),
        2,
        "file_a 的反向索引应只剩 f1/f3"
    );

    // 关键回归点：删除后再加节点/边，新边必须连在预期节点上
    let f6 = make_function("zeta", file_b, "Rust");
    graph.add_function(f6.clone());
    graph
        .add_call_relation(make_call(&f6, &f3, 20))
        .expect("new node must be resolvable");
    graph
        .add_call_relation(make_call(&f4, &f6, 21))
        .expect("new edge must connect expected nodes");

    assert_call_graph_invariants(&graph, "删除后重新添加节点/边");
    assert_eq!(
        names_of(graph.get_callees(&f6.id)),
        expected_names(&["gamma"]),
        "新节点 f6 的出边应指向 gamma"
    );
    assert_eq!(
        names_of(graph.get_callers(&f3.id)),
        expected_names(&["zeta"]),
        "f3 的入边应来自新节点 zeta"
    );
    assert_eq!(
        names_of(graph.get_callees(&f4.id)),
        expected_names(&["zeta"]),
        "f4 的出边应只指向新节点 zeta"
    );
    println!(
        "删除 f2 后重建: {} 节点 / {} 边 / total_functions={}",
        graph.graph.node_count(),
        graph.graph.edge_count(),
        graph.stats.total_functions
    );
}

// ---------------------------------------------------------------------------
// 测试 2：remove_functions_by_file 的统计与索引
// ---------------------------------------------------------------------------

#[test]
fn test_remove_functions_by_file_keeps_invariants_and_stats() {
    let file_a = "src/a.rs";
    let file_b = "src/b.rs";

    // 重载同名函数，验证 function_names 反向索引清理
    let a1 = make_function("shared", file_a, "Rust");
    let a2 = make_function("only_a", file_a, "Rust");
    let a3 = make_function("alpha3", file_a, "Rust");
    let b1 = make_function("shared", file_b, "Python");
    let b2 = make_function("beta2", file_b, "Python");
    let b3 = make_function("gamma3", file_b, "Rust");

    let mut graph = PetCodeGraph::new();
    for f in [&a1, &a2, &a3, &b1, &b2, &b3] {
        graph.add_function(f.clone());
    }
    for (caller, callee) in
        [(&a1, &a2), (&a2, &a3), (&a3, &b1), (&b1, &b2), (&b2, &b3), (&b3, &a1)]
    {
        graph
            .add_call_relation(make_call(caller, callee, 7))
            .expect("add_call_relation should succeed");
    }
    graph.update_stats();
    assert_call_graph_invariants(&graph, "初始状态");
    assert_eq!(graph.stats.total_files, 2, "初始应有 2 个文件");
    assert_eq!(graph.find_functions_by_name("shared").len(), 2, "同名重载应有 2 项");

    graph.remove_functions_by_file(&PathBuf::from(file_a));

    assert_call_graph_invariants(&graph, "删除 file_a 之后");
    assert_eq!(graph.graph.node_count(), 3, "应只剩 file_b 的 3 个函数");
    assert_eq!(
        graph.stats.total_functions, 3,
        "total_functions 应按实际删除数递减，不能重复扣/少扣"
    );
    assert!(
        graph.find_functions_by_file(&PathBuf::from(file_a)).is_empty(),
        "file_a 的正向查询应为空"
    );
    assert!(
        !graph.file_functions.contains_key(&PathBuf::from(file_a)),
        "空的 file_functions key 应被移除"
    );
    assert_eq!(
        graph.find_functions_by_name("shared").len(),
        1,
        "同名重载中只应剩 file_b 的那一个"
    );
    assert_eq!(
        graph.find_functions_by_name("only_a").len(),
        0,
        "被删文件的函数名索引应清空"
    );
    assert_eq!(graph.stats.languages.get("Rust"), Some(&1), "Rust 只剩 gamma3");
    assert_eq!(graph.stats.languages.get("Python"), Some(&2), "Python 仍为 2");
    graph.update_stats();
    assert_eq!(graph.stats.total_files, 1, "update_stats 后 total_files 应为 1");
    println!(
        "删除整文件后: {} 节点 / {} 边 / languages={:?}",
        graph.graph.node_count(),
        graph.graph.edge_count(),
        graph.stats.languages
    );
}

// ---------------------------------------------------------------------------
// 测试 3：update_stats 纠正历史虚高的调用计数
// ---------------------------------------------------------------------------

#[test]
fn test_update_stats_recomputes_call_counts() {
    let f1 = make_function("one", "src/x.rs", "Rust");
    let f2 = make_function("two", "src/x.rs", "Rust");
    let f3 = make_function("three", "src/y.rs", "Rust");

    let mut graph = PetCodeGraph::new();
    for f in [&f1, &f2, &f3] {
        graph.add_function(f.clone());
    }
    graph
        .add_call_relation(make_call(&f1, &f2, 1))
        .expect("resolved edge");
    graph
        .add_call_relation(make_call(&f2, &f3, 2))
        .expect("resolved edge");
    let mut unresolved = make_call(&f3, &f1, 3);
    unresolved.is_resolved = false;
    graph
        .add_call_relation(unresolved)
        .expect("unresolved edge");

    // 模拟历史持久化文件里的虚高数字
    graph.stats.resolved_calls += 100;
    graph.stats.unresolved_calls += 7;
    graph.update_stats();

    assert_eq!(graph.stats.resolved_calls, 2, "update_stats 应重算 resolved_calls");
    assert_eq!(
        graph.stats.unresolved_calls, 1,
        "update_stats 应重算 unresolved_calls"
    );
    assert_call_graph_invariants(&graph, "update_stats 纠正之后");

    // 删除一个节点后计数应同步下降，且 update_stats 结果一致
    assert!(graph.remove_function(&f1.id));
    assert_call_graph_invariants(&graph, "删除 f1 之后");
    let before = (graph.stats.resolved_calls, graph.stats.unresolved_calls);
    graph.update_stats();
    assert_eq!(
        (graph.stats.resolved_calls, graph.stats.unresolved_calls),
        before,
        "增量维护的计数应与全量重算一致"
    );
    println!(
        "删除 f1 后 resolved/unresolved = {} / {}",
        graph.stats.resolved_calls, graph.stats.unresolved_calls
    );
}

// ---------------------------------------------------------------------------
// 测试 4：EntityGraph 的 swap 搬移不变量
// ---------------------------------------------------------------------------

#[test]
fn test_entity_graph_remove_keeps_index_mapping_consistent() {
    let file_a = "src/e1.rs";
    let file_b = "src/e2.rs";

    let c1 = make_class("Cls1", file_a, "Rust");
    let c2 = make_class("Cls2", file_a, "Rust");
    let c3 = make_class("Cls3", file_b, "Rust");
    let m1 = make_function("Cls1_m1", file_a, "Rust");
    let m2 = make_function("Cls3_m2", file_b, "Rust");

    let mut graph = EntityGraph::new();
    for c in [&c1, &c2, &c3] {
        graph.add_class(c.clone());
    }
    for f in [&m1, &m2] {
        graph.add_function(f.clone());
    }
    graph
        .add_edge(entity_edge(c1.id, m1.id, EntityEdgeType::Contains))
        .expect("contains edge");
    graph
        .add_edge(entity_edge(c1.id, c2.id, EntityEdgeType::Inherits))
        .expect("inherits edge");
    graph
        .add_edge(entity_edge(c3.id, m2.id, EntityEdgeType::Contains))
        .expect("contains edge 2");
    graph
        .add_edge(entity_edge(c2.id, c3.id, EntityEdgeType::Imports))
        .expect("imports edge");
    graph.update_stats();

    assert_entity_graph_invariants(&graph, "初始状态");
    assert_eq!(graph.stats.total_classes, 3);
    assert_eq!(graph.stats.total_functions, 2);

    // 删除非末尾的类（index 0，last = 4）→ 触发 swap 搬移
    assert!(graph.remove_entity(&c1.id), "删除已存在实体应返回 true");
    assert!(!graph.remove_entity(&c1.id), "重复删除应返回 false");
    assert!(!graph.remove_entity(&Uuid::new_v4()), "删除不存在实体应返回 false");

    assert_entity_graph_invariants(&graph, "删除 c1 之后");
    assert_eq!(graph.graph.node_count(), 4, "应剩 2 类 + 2 函数");
    assert_eq!(graph.stats.total_classes, 2, "total_classes 应递减");
    assert_eq!(graph.stats.total_functions, 2, "删除类不应影响 total_functions");
    assert!(
        graph.find_classes_by_name("Cls1").is_empty(),
        "class_names 应清掉被删 id"
    );
    assert_eq!(
        graph.find_classes_by_name("Cls3").len(),
        1,
        "被搬移的 Cls3 仍应能按名查到"
    );
    assert!(
        graph
            .get_class_by_id(&c3.id)
            .map(|c| c.name == "Cls3")
            .unwrap_or(false),
        "swap 搬移后按 id 取回的类不应错位"
    );
    assert_eq!(
        graph.graph.edge_count(),
        2,
        "c1 的两条出边级联消失，应剩 c2->c3 与 c3->m2"
    );

    // 删除函数节点：m2 因上一步 swap 已位于索引 0（非末尾），必然再次触发搬移
    let m2_index_before = graph.entity_to_node[&m2.id];
    assert!(graph.remove_entity(&m2.id));
    assert_entity_graph_invariants(&graph, "删除被搬移到索引 0 的 m2 之后");
    assert_eq!(graph.stats.total_functions, 1, "total_functions 应递减");
    assert!(
        graph.get_entity_by_id(&m2.id).is_none(),
        "被删实体不应还能按 id 取到"
    );
    assert_eq!(
        graph.graph.edge_count(),
        1,
        "c3->m2 的边应随 m2 级联消失，只剩 c2->c3"
    );

    // m1 是被 swap 搬移到 m2 旧索引处的节点，按 id 仍必须取回自己
    let m1_name = graph
        .get_entity_by_id(&m1.id)
        .and_then(|node| match node {
            EntityNode::Function(f) => Some(f.name.clone()),
            _ => None,
        })
        .unwrap_or_else(|| {
            panic!(
                "m1 的映射被腐蚀：它应占据 m2 腾空后的索引 {}",
                m2_index_before.index()
            )
        });
    assert_eq!(
        m1_name, "Cls1_m1",
        "swap 搬移后按 id 取回的函数不应错位"
    );

    // 删除后新增实体并连边，验证映射未被腐蚀
    let c4 = make_class("Cls4", file_b, "Rust");
    graph.add_class(c4.clone());
    graph
        .add_edge(entity_edge(c4.id, m1.id, EntityEdgeType::Contains))
        .expect("new entity must be resolvable");
    assert_entity_graph_invariants(&graph, "删除后重新添加实体/边");
    assert_eq!(
        graph
            .get_class_members(&c4.id)
            .iter()
            .map(|f| f.name.clone())
            .collect::<Vec<_>>()
            .len(),
        0,
        "member_functions 未更新，此处只验证不 panic"
    );
    println!(
        "EntityGraph 最终: {} 节点 / {} 边 / classes={} functions={}",
        graph.graph.node_count(),
        graph.graph.edge_count(),
        graph.stats.total_classes,
        graph.stats.total_functions
    );
}

// ---------------------------------------------------------------------------
// 测试 5：循环压力（反复删除中间节点，每步校验不变量）
// ---------------------------------------------------------------------------

#[test]
fn test_pressure_repeated_middle_removals() {
    const N: usize = 12;

    let mut graph = PetCodeGraph::new();
    let mut funcs = Vec::new();
    for i in 0..N {
        let file = if i % 2 == 0 { "src/even.rs" } else { "src/odd.rs" };
        funcs.push(make_function(&format!("fn_{}", i), file, "Rust"));
    }
    for f in &funcs {
        graph.add_function(f.clone());
    }
    for i in 0..N - 1 {
        graph
            .add_call_relation(make_call(&funcs[i], &funcs[i + 1], i))
            .expect("chain edge");
    }
    graph
        .add_call_relation(make_call(&funcs[N - 1], &funcs[0], 999))
        .expect("closing edge");
    graph
        .add_call_relation(make_call(&funcs[3], &funcs[3], 777))
        .expect("self loop edge");

    // 自环只能计一次（Outgoing/Incoming 双计会虚高）
    assert_eq!(
        graph.stats.resolved_calls,
        graph.graph.edge_count(),
        "自环不应被双计"
    );
    assert_call_graph_invariants(&graph, "压力用例初始");

    let mut remaining: Vec<Uuid> = funcs.iter().map(|f| f.id).collect();
    let mut rounds = 0usize;
    while remaining.len() > 1 {
        let victim_pos = remaining.len() / 2;
        let victim = remaining.remove(victim_pos);
        assert!(
            graph.remove_function(&victim),
            "第 {} 轮删除中间节点应成功",
            rounds
        );
        assert_call_graph_invariants(&graph, &format!("压力用例第 {} 轮", rounds));
        assert_eq!(
            graph.graph.node_count(),
            remaining.len(),
            "第 {} 轮后节点数应与剩余 id 数一致",
            rounds
        );
        rounds += 1;
    }

    assert_eq!(graph.graph.node_count(), 1, "应只剩 1 个节点");
    assert_eq!(graph.stats.total_functions, 1, "total_functions 应收敛到 1");
    assert_eq!(
        graph.stats.resolved_calls, graph.graph.edge_count(),
        "最终调用计数应与实际边数一致"
    );
    // 幸存节点仍可按 id 查到
    let survivor = remaining[0];
    assert!(
        graph.get_function_by_id(&survivor).is_some(),
        "幸存节点的映射必须完好"
    );
    graph.update_stats();
    assert_call_graph_invariants(&graph, "压力用例最终 update_stats 之后");
    println!(
        "压力用例完成 {} 轮删除，最终 {} 节点 / {} 边",
        rounds,
        graph.graph.node_count(),
        graph.graph.edge_count()
    );
}

// ---------------------------------------------------------------------------
// 测试 6：对照用例 —— 证明上面的不变量确实能抓到旧的"手写删除"
// ---------------------------------------------------------------------------

#[test]
fn test_unsafe_removal_would_break_mapping() {
    let mut graph = PetCodeGraph::new();
    let funcs: Vec<FunctionInfo> = (0..4)
        .map(|i| make_function(&format!("g{}", i), "src/g.rs", "Rust"))
        .collect();
    for f in &funcs {
        graph.add_function(f.clone());
    }

    let victim = funcs[1].id;
    let node_index = graph.get_node_index(&victim).expect("node must exist");

    // 复现旧的手写删除：只删自己那一条映射，不修正被 swap 搬移到 node_index 的末位节点
    graph.graph.remove_node(node_index);
    graph.function_to_node.remove(&victim);
    graph.node_to_function.remove(&node_index);

    // 末位节点 g3 的数据已搬走到 node_index，但 entity_to_node/function_to_node 仍记着旧索引
    let last_function = &funcs[3];
    let stale_index = graph
        .function_to_node
        .get(&last_function.id)
        .copied()
        .expect("旧写法不会删掉末位节点的映射项");
    let weight_at_stale = graph.graph.node_weight(stale_index).map(|f| f.id);
    assert!(
        weight_at_stale != Some(last_function.id),
        "对照用例应能复现映射腐蚀：末位节点 {} 的缓存索引 {:?} 处不该是它自己（实际 {:?}）",
        last_function.id,
        stale_index,
        weight_at_stale
    );
    println!(
        "旧写法复现成功：{} 的缓存索引 {:?} 已指向别的节点/越界",
        last_function.name, stale_index
    );

    // 用新的索引安全原语删除同一场景，映射必须保持完好
    let mut safe = PetCodeGraph::new();
    for f in &funcs {
        safe.add_function(f.clone());
    }
    assert!(safe.remove_function(&victim), "新原语应成功删除已存在节点");
    assert_call_graph_invariants(&safe, "新原语对照组");
    assert_eq!(
        safe.get_function_by_id(&last_function.id)
            .expect("末位节点应仍可按 id 取到")
            .name,
        "g3",
        "新原语必须修正被搬移节点的映射"
    );
}
