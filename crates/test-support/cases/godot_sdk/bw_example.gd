# The Godot example (BW-21-05-04): a node logs in, syncs a property and,
# when it leaves the scene, logs out, disconnects the signal and cleans up.
extends Node

var _sdk = BwGodot.new()
var _target = load("res://addons/boundless_sdk/bw_signal_target.gd").new()


func _ready() -> void:
    _target.connect_to_server(_sdk)
    _sdk.login("alice", "secret")
    _sdk.set_property(1, "position.x", "1.5")


func _exit_tree() -> void:
    _sdk.logout()
    _target.disconnect_from_server(_sdk)
