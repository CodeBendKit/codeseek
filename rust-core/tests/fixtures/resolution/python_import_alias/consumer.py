"""场景 2（import 别名）：`func` 被导入并改名成 `alias`，调用点写的是 `alias`。

当前实现完全不消费 `ImportDeclaration`（`parser.rs` 两处 `symbol_type()` match 的
`_ => {}` 分支直接丢弃），所以 `alias` 匹配不到任何函数名。
"""
from mod_x import func as alias


def call_alias(value):
    return alias(value)
