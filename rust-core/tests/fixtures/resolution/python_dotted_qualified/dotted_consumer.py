"""场景 4（点号限定名）：调用点写成 `mod_y.helper(value)`，期望解析到 mod_y 里的那份。

正确解析结果：`call_dotted -> mod_y.helper`（而不是 `other_mod.helper`）。
"""
import mod_y
import other_mod


def call_dotted(value):
    return mod_y.helper(value)
