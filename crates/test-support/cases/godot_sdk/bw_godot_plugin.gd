# The editor plugin named by plugin.cfg (BW-21-05-01): it makes the SDK
# script an autoload, so every node can reach it by name.
extends EditorPlugin


func _enter_tree() -> void:
    add_autoload_singleton("BoundlessSdk", "res://addons/boundless_sdk/bw_sdk.gd")


func _exit_tree() -> void:
    remove_autoload_singleton("BoundlessSdk")
