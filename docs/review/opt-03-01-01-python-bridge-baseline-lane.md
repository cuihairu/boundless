# OPT-03-01-01 测量 Python 桥接基线

对应 todo.md OPT-03-01-01：**测量 Python 桥接基线。验收：保存转换、GIL 和回调耗时。**

交付面：
- `crates/python-host/tests/bridge_baseline.rs`：一个集成 target（`the_bridge_baseline_records_all_faces`），对真 CPython（crate 手声明 C API 所链的 3.x 解释器）测四族桥接面的每操作耗时，结构断言随行（每面往返精确值、回调返回和、post 顺序与完成数）——是测试不是探针。
- 记录 TSV：`docs/review/fixtures/python-baseline/bridge-baseline.tsv`（PYTHON_BRIDGE_OUT 落盘，追加式）。

## 方法

单操作 C API 成本在几十纳秒量级，逐操作 Instant 计时的自身开销比操作本身还大，不诚实。因此每面按**窗测**：固定 1000 次背靠背操作整窗计时，窗口除以次数得每操作摊派值，50 窗（回调族 10 窗）取 p50/p90/p99/max。debug build，Linux 本机，数字机器相关只记录不设带。

| 族 | 面 | 每操作 p50 / p99（ns） |
|---|---|---|
| native→Python | `PyLong_FromLong` | 17 / 37 |
| | `PyFloat_FromDouble` | 34 / 65 |
| | `PyUnicode_FromStringAndSize`（35B） | 115 / 486 |
| | `PyTuple_New`+3×SetItem | 206 / 3249 |
| Python→native | `PyLong_AsLongLong` | 25 / 44 |
| | `PyFloat_AsDouble` | 25 / 46 |
| | `PyUnicode_AsUTF8AndSize` | 32 / 199 |
| | `PyTuple_Size`+`GetItem`+`AsLongLong` | 43 / 147 |
| 句柄层 | `PyRef::clone`+drop（INCREF/DECREF 对） | 96 / 698 |
| GIL 括号 | `release_lock`+`acquire_lock`（无竞争对） | 230 / 6497 |
| 回调 | `PyObject_CallNoArgs`（probe） | 111 / 215 |
| | `PyObject_CallObject`（3 参 sum） | 116 / 1737 |
| 主线程任务 | `gil::post`×500 后单次 `process` 摊派 | 160 / 2446 |

## 结论

1. **标量转换是真实零开销量级**：int/float 双向 17–34ns，比 GIL 括号（230ns）和回调（111–116ns）都便宜一个数量级——OPT-03-01-02「优化转换」的收益上限在标量面上极薄，真正值得动的是**字符串/复合对象面**（115–206ns，含 UTF-8 转码/容器分配）与句柄层（96ns 的 INCREF/DECREF 对已是下限附近）。
2. **回调开销不在调用本身**：`PyObject_CallObject` 3 参 116ns 与 noargs 111ns 几乎持平——参数打包（build tuple+3×FromLong）不在测量内的话调用面已贴 CPython 下限；OPT-03-01-03「跨语言回调」的优化空间在**排队/调度侧**（post+drain 摊派 160ns，其中 process 快照+catch_unwind 包装是 port 侧新增成本）而非 C API 调用侧。
3. **GIL 括号 230ns 无竞争**：参考引擎的 main-thread idle 对（SaveThread/RestoreThread）本身极便宜，尾延迟（p99 6.5µs）来自偶发调度抖动；有竞争场景未测（本面无第二线程状态面，`PyThreadState_New` 不在 capi 面）。
4. 结构断言全绿：八面往返精确值、回调返回 6、500 post 全部按序完成、`pending()==0` 收尾。
5. 首跑/复跑形态一致（三连跑 0.09–0.14s），结构性断言逐跑恒等，墙钟数字按窗分布同量级。

## 复跑命令（canonical）

```sh
PYTHON_BRIDGE_OUT=<绝对路径> cargo test -p python-host \
    --test bridge_baseline the_bridge_baseline_records_all_faces -- --exact
```

TSV 追加落盘，重跑前先清文件。

## 门禁（全绿后落账）

- `cargo test -p python-host`：全 target 绿（含 crate 自带场景进程族）。
- clippy `-D warnings`、rustfmt `--edition 2024 --check`：净（workspace manifest 的 blowfish 未用警告是继承债）。

## 坑集（复现与规避）

- **逐操作 Instant 对几十 ns 的面是假数**：窗口整批计时再摊派，p50/p99 才是可信量级。
- **`PyObject_CallNoArgs` 不是零参万能调用**：被调函数必须真无参——对 `add3(a,b,c)` 用它拿到 null 返回且报错滞留到解释器关闭才喷（"Exception ignored on threading shutdown"）；备一个 `probe()` 无参函数专用。
- **`PyTuple_SetItem` 偷引用**：复合面循环装填时每元素先 INCREF 再 SetItem，否则窗口循环把元素引用计数打穿。
- **解释器每进程一次**：集成 target 只放一个测试函数，本进程独占解释器，不需要 bw_profile 那套子进程场景编排。
- **to_python 各面的 decref 就在计时环里**：产品逐个 `Py_DecRef` 是测量的一部分（~10ns，要承认它计入了）；想剥出去就得攒一批环外统一清理，本面选择承认它。
