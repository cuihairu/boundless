# BW-16-04-03 定义 Rust 扩展入口 — 入口、成功与错误传播

- **任务编号**: BW-16-04-03（lane BW-16 源码映射与扩展接口，todo.md 行 1411）
- **验收**: 成功及错误能正确传播。
- **结论**: 满足并勾行。Rust 原生扩展入口（`crates/plugin-host` 的 `PluginLoader` 移植，
  参考 `tools/plugin_system/plugin_loader.{hpp,cpp}`）已随 BW-13-06-03 合入：`loadPlugin`
  仅当插件 `init` 入口存在且返回真时保留（成功传播）；缺库、缺 init 入口、init 返回假、
  缺插件列表均不残留（错误传播）；`unloadPlugin` 先跑 `fini` 且 `fini` 返回假可否决卸载；
  `finiPlugins` 逆序卸载。14 个加载器测试全绿。

## 1. 入口与契约

`crates/plugin-host/src/loader.rs`，`dlopen`/`dlsym` 移植参考
`tools/plugin_system/plugin_loader.{hpp,cpp}`：

| 参考行为 | 新版实现 | 测试 |
|---|---|---|
| `loadPlugin` 保留插件仅当 init 存在且返回 true | `load_plugin`（成功传播） | `a_plugin_that_initialises_is_loaded_and_kept` |
| 缺库 → 打开失败，无残留 | `load_plugin` 返回 open 错误 | `a_missing_library_is_an_open_failure` |
| 缺 init 入口 → 回滚（仍跑 fini 若有） | `load_plugin` 回滚 | `a_library_without_the_requested_init_is_rolled_back` |
| init 返回 false → 不注册 | `load_plugin` 拒绝 | `a_plugin_whose_init_refuses_is_not_registered` |
| `unloadPlugin` 先 fini，fini 返回 false 否决卸载 | `unload_plugin` | `a_plugin_that_refuses_to_unload_stays_loaded` |
| `finiPlugins` 逆序卸载 | `finish` | `finishing_unloads_in_reverse_load_order` |
| 插件列表逐条加载并报告失败 | `load_list` | `the_plugin_list_loads_each_entry_and_reports_failures` |

错误面经 `error_variants_name_their_failure_mode` 逐变体核对。

## 2. 实测（14 测全绿）

```
$ cargo test -p plugin-host
test a_plugin_that_initialises_is_loaded_and_kept ... ok
test a_missing_library_is_an_open_failure ... ok
test a_library_without_the_requested_init_is_rolled_back ... ok
test a_plugin_whose_init_refuses_is_not_registered ... ok
test unload_releases_a_loaded_plugin ... ok
test a_plugin_that_refuses_to_unload_stays_loaded ... ok
test unloading_an_unknown_index_is_a_no_op_success ... ok
test a_plugin_without_a_fini_entry_point_unloads_anyway ... ok
test finishing_keeps_a_plugin_that_vetoes_unloading ... ok
test finishing_unloads_in_reverse_load_order ... ok
test the_plugin_list_loads_each_entry_and_reports_failures ... ok
test a_missing_plugin_list_is_an_error ... ok
test the_debug_symbol_pair_names_the_d_entry_points ... ok
test error_variants_name_their_failure_mode ... ok
test result: ok. 14 passed; 0 failed
```

## 3. 边界

本项只覆盖**入口**及其成功/错误传播（即验收范围）。`editor/tools` 应用（消费该入口的
宿主侧）未建，属 BW-13（编辑器/工具立项）；入口已就绪，随其交付即可挂接。
`appmgr` 参考目标的 `AppConfig`/`ServerApp` 面不在本项（那是服务端运行时入口，属
BW-06..10）。