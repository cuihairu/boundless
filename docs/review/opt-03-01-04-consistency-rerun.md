# OPT-03-01-04 复跑引用、异常及重载用例

对应 todo.md OPT-03-01-04：**复跑引用、异常及重载用例。验收：行为一致。**

定位：OPT-03-01-02（math 注册表换写时复制快照）与 OPT-03-01-03（回调队列加原子计数空转快路径）动了 python-host 最热的两条行为路径。本行在优化后的树上**逐名复跑**覆盖引用计数、异常路径与重载机制的三个用例族，外加原版 math 回归套件，验收=全部与优化前行为一致（零失败、零语义断言改动）。

## 复跑清单与结果（2026-11-26，优化后树 tip `ca9cbaa`）

15 个场景逐名 `--ignored --exact` 复跑，**15/15 全绿**：

引用族（5）：
- `scenario_a_handle_owns_one_reference`
- `scenario_the_cache_holds_one_reference_per_identity`
- `scenario_a_handle_outliving_finalisation_is_safe`
- `scenario_different_ids_are_distinct_wrappers`
- `scenario_the_same_native_id_wraps_to_one_object`

异常族（5）：
- `scenario_an_exception_in_a_callback_does_not_leave_it_queued`（回调内 raise 不滞留队列——与 03-01-03 快路径直接相关）
- `scenario_the_excepthook_formats_the_traceback`
- `scenario_the_excepthook_handles_a_missing_traceback`
- `scenario_bad_write_arguments_raise`
- `scenario_a_failed_source_reports_failure`

重载族（4）：
- `scenario_replacing_a_script_class_updates_existing_instances`
- `scenario_swapping_in_a_module_window_keeps_sys_and_main`
- `scenario_migrate_reports_the_hook_success_and_failure`
- `scenario_a_failed_reload_leaves_the_old_definitions_callable`

原版回归（1）：
- `scenario_the_original_math_regressions_run_against_the_port`（原版 math 回归套件对移植面全量跑——与 03-01-02 转换优化直接相关）

## 结论

1. **行为一致达成**：三个用例族 + 原版 math 回归全绿，优化期间未改任何行为断言（03-01-02 只换注册表内部结构、03-01-03 只加读侧快路径，快照/计数在锁下与队列同步维护，语义文档随码注明）。
2. 两处与优化直接相关的行为面（回调异常不滞留、原版 math 回归）逐名复跑确认不受快路径/快照影响。
3. 全量场景面另有 `every_scenario_exits_cleanly`（50 场景出进程跑）在默认 `cargo test -p python-host` 里随每轮门禁绿。

## 复跑命令（canonical）

```sh
cargo test -p python-host --lib -- --ignored --exact \
    interpreter::tests::scenario_<name>
```

## 门禁（全绿后落账）

- 上列 15 场景逐名绿；`cargo test -p python-host` 全 target 绿（lib 151 + bridge_baseline 1）。
