"""``python -m mtebcode_eval`` 入口。

只做一件事：把 ``cli.main`` 的返回值转成进程退出码，保证 shell / CI 能判断成败。
"""

from __future__ import annotations

import sys

from .cli import main

if __name__ == "__main__":
    sys.exit(main())
