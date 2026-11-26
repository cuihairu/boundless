# OPT-07-01-04 复跑接管

Acceptance: **恢复实体和共享状态正确**。本行是复跑行（零代码改动）：把接管（BaseApp/管理器死亡→恢复）两半的测试族全量复跑，核对每条结果与记录的参考契约一致。

## 复跑面与结果（2026-11-26 本机）

### 实体恢复（备份接管 + 归档重启 + offload 恢复）

- `base_backup.rs` 恢复族：`a_stored_backup_restores_matching_properties`（存下的备份重建出属性一致的实体）、`only_the_dead_apps_backups_are_restored`（只恢复失联 app 的备份）、`a_truncated_backup_fails_the_restore`（截断备份恢复失败）、`the_standard_entity_backup_has_a_pinned_wire_size`（181 wire 字节+回读重建）。
- `offloaded_backup.rs`：`a_death_clears_every_backup`、`a_death_of_an_unknown_app_still_clears`（死亡清账两臂）。
- `archiver.rs`：`restart_recovers_the_persisted_state`（重启从 ArchiveStore 读回重建）、`an_unpersisted_id_has_nothing_to_restore`。
- `offload_recovery.rs` 13 测全绿（`a_lost_target_recovers_to_a_consistent_role_and_message_state`、`the_remaining_ghosts_repoint_at_the_restored_real`、`restoring_onto_the_dead_cell_is_rejected` 等一致角色/消息状态恢复面）。

### 共享状态恢复（管理器死亡 + 权威 map + 传播序）

- `baseappmgr.rs`：`recovery_restores_the_authoritative_maps`（死亡后权威 map 从流恢复）、`shared_data_reaches_every_base_app`、`shared_data_is_kept_for_new_apps`（迟入 app 同步）、`del_shared_data_follows_the_same_order`（删除同序传播）、`the_shared_data_message_volume_is_one_per_subscriber`（消息量台账，每订阅者恰一条不变）。
- `cellappmgr.rs`：`recovery_restores_the_authoritative_maps`（CELL_APP/GLOBAL 权威恢复同形）。
- 附加 `dbappmgr::recover_db_app_keeps_ids`（DBApp id 保持）。

## 行为一致判读

接管两半全部复跑与交付时记录的参考契约逐条一致：实体半（备份字节回读重建同一实体、只恢复失联 app、截断拒绝）与共享半（权威 map 流恢复、迟入同步、每订阅者恰一条、删除同序）。`read_backup_data`/`restore` 路径在 OPT-07-01-02 单遍化后行为测全不动（字节等价既有测全绿），复跑再证零回归。**零代码改动、零回归**。

## 门禁

- `cargo test -p server-runtime --lib`：**1703 passed / 0 failed / 1 ignored**。
- 过滤族：`-- backup recovery shared_data restart` 63/63、`-- restart recovery shared_data offloaded` 37/37（族交集覆盖上述全部测名）。
- `--test el7_mixed_deploy -- --ignored the_mixed_deploy_rerun_passes_all_legs`：**1/1 绿**（EL7 原版二进制双向 5 腿集群级恢复面）。
