# BW-00-04-02 — 采集 Python 回调夹具

验收：保留顺序、参数与可忽略字段。

本任务把参考栈调用 Python 回调时的**顺序、参数与返回值**固化为基准样本。
回调机制用的是参考栈自身的 `ScriptEventList` / `ScriptEvents`
（`lib/pyscript`）：注册的 Python 监听器按 level 顺序（同 level 稳定保持
插入序）被触发，每个监听器收到触发时的参数元组，返回值落入 results 列表。
全部监听器是参考内嵌 Python 3.13 执行的真正 Python 可调用对象，采集到的
顺序/参数/返回即原版行为，不是重实现。

- 参考栈：BigWorld 4657a17，`programming/bigworld` 预编译静态库
  （`build/cmake-entitydef/libscript.a`、`libpyscript.a`、内嵌
  `bwpython313/libpython3.13.a`），x86-64 Linux。
- 采集工具：`docs/review/tools/capture_callbacks.cpp`，编译命令见文件头注释
  （`--root` 传给 `BaseResMgrUnitTestHarness`，与参考单元测试一致）。
- 原始输出：`docs/review/data/bw00-04-02-logs/capture.txt`。
- 基准样本：`docs/review/fixtures/callbacks/callback-trace.txt`。
- 确定性：两次运行 stdout 完全一致。

## 场景

三个监听器注册到同一个 `ScriptEventList`：

| 监听器 | level | 行为 |
|--------|-------|------|
| a | -1 | 记录 `('a', arg)`，返回 `'A-return'` |
| b | 0 | 记录 `('b', arg)`，返回 `None` |
| c | 0 | 记录 `('c', arg)`，返回 `arg`（回显） |

b、c 同 level 0，c 后注册，稳定序为 a(-1) → b(0) → c(0)。

## 各用例

### int-arg — `triggerEvent((42,))`

- **顺序** `a b c`（level 序，同 level 稳定插入序）。
- **参数**：每个监听器收到 `42`。
- **返回**：`['A-return', None, 42]`（b 返回 None，c 回显参数）。

### str-arg — `triggerEvent(('hello',))`

- 顺序 `a b c`；参数 `'hello'`；返回 `['A-return', None, 'hello']`。

### tuple-arg — `triggerEvent(((1, 2, 3),))`

- 顺序 `a b c`；参数 `(1, 2, 3)`；返回 `['A-return', None, (1, 2, 3)]`。

### dict-arg — `triggerEvent(({'k': 7},))`

- 顺序 `a b c`；参数 `{'k': 7}`；返回 `['A-return', None, {'k': 7}]`。

### none-arg — `triggerEvent((None,))`

- 顺序 `a b c`；参数 `None`；返回 `['A-return', None, None]`。

### timestamp-arg-IGNORABLE — `triggerEvent(({'ts': <float>},))`

- 顺序 `a b c`；参数 `{'ts': 0.123456789}`；返回
  `['A-return', None, {'ts': 0.123456789}]`。
- **可忽略字段**：`ts` 值是运行时浮点（此处为固定注入值，真实运行随时间
  变化）。差异报告须把 `ts` 标记为可忽略（IGNORABLE），不参与比较——
  事件名带 `-IGNORABLE` 后缀，样本中以 `<float>` 占位。

## 不变式（差异报告应断言的）

1. **顺序**：任何一次运行，监听器触发起序均为 `a b c`，不得颠倒。
2. **参数**：每个监听器收到的参数等于触发时传入的参数（按位置）。
3. **返回**：results 与监听器一一对应，位置一致（`None` 表示无返回值或
   抛异常）。
4. **可忽略**：`ts` 字段不比较。
