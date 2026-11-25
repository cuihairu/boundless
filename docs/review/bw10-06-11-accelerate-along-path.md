# BW-10-06-11 实现 accelerateAlongPath（多段路径拐点的减速与终点行为与原版一致）

**验收**：多段路径拐点的减速与终点行为与原版一致。

## 参照面链

`EntityNavigate::accelerateAlongPath`（entity_navigate.cpp:343-379），
三查按序：`!entity_.isRealToScript()` → TypeError 无前缀共享文本 →
`waypoints.empty()` → TypeError **`"Waypoints must not be empty."`** →
facing 越界 → TypeError **`"Facing must one of the value 0, 1 or 2"`**
（参照自带 typo）。全过后
`new AccelerateAlongPathController( waypoints, acceleration, maxSpeed,
facing )` → `addController(pController, userArg)` → id。**无载具
检查**；控制器构造器内还有一道 `MF_ASSERT( !waypoints_.empty() )`，
经此脸不可达（端口留 debug_assert）。**无 stopAtDestination 参数**
（与 accelerateToPoint 不同）——路径脸中间点巡航（stop=false）、
只有末点停（stop=true）。

控制器本体 `AccelerateAlongPathController`
（accelerate_along_path_controller.cpp:47-135）**BW-08-02-03 已交付**
（movement.rs，update: 末点判定→move→progress++→末点 onMove 自取消；
streamIO 含 waypoints 数量+progress），本项只补脚本脸。

## 怪癖（钉死）

1. **三查顺序**：real → empty → facing——空表 + 坏 facing 时
   EmptyWaypoints 先赢；ghost 赢一切。
2. **拐点无减速无回调**：中间点 `stop=false` 使 desired velocity 恒
   max_speed（不进 sqrt(2·d·a) 制动曲线），move 吸附过点后
   `progress++`、**不进日志**；只有末点 `stop=true` 制动 + `onMove`
   自取消。
3. **采样速度滞后一拍**（06-10 已钉）让首段起步慢：实证 3 点路径
   (1,0,0)→(2,0,0)→(5,0,0)（acc 2/max 2/hz 10）——**拐点 1 在
   tick 14（x=1.0）、拐点 2 在 tick 22（x=2.0）、两处日志均空**；
   末段制动后吸附 (5,0,0)、state.velocity 衰减 0、单条 onMove
   （id 5/userArg 3）、progress=3。

## 验收（实证）

- **巡航拐点 + 终点行为**：progress 0→1→2 递进（tick 14/22 拐点
  快照：位置=路点本身、log.len()==0）；末点吸附+速度归零+onMove。
- **拒绝序**：空表（连 facing 7 一起喂，EmptyWaypoints 赢）→
  facing 3（BadFacing 逐字）→ ghost（NotReal 无前缀逐字）。

## 未盖行（共 10 行，99.58%）

与 06-02..06-10 相同的防御集（行号随插入平移）：
`DestinationUnresolved` 返回 + message 臂 ×2 处、`canNavigateTo`
目的地非法臂、06-01 失败路线永不重入循环守卫 2 行、
`SafetyCountExceeded` 返回 + message 臂。**06-11 新增代码全覆盖**
（`AccelerateAlongPathFailure` 三条 message 臂全由拒测击中）。

## 门禁

- `cargo test -p server-runtime`：**1160 过 + 1 忽略**（含 2 条新
  accelerateAlongPath 测试，模块共 39）。
- `cargo test --workspace`：**4460 过 / 0 失败**。
- `cargo fmt -p server-runtime -- --check`、
  `cargo clippy -p server-runtime --all-targets -- -D warnings` 全绿。
- 覆盖（llvm-cov 0.9.1 `--lcov`，DA max 合并，`cargo clean -p` 后测）：
  `entity_navigate.rs` **99.58%**（2388/2398）。

证据测试：`crates/server-runtime/src/entity_navigate.rs`
（accelerate_along_path 脸 + AccelerateAlongPathFailure + 2 测试）；
`crates/server-runtime/src/movement.rs`（控制器本体既有测试）。
