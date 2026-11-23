# The SDK script (BW-21-05-02): a script logs in, issues an RPC and logs
# out against the native class, one script method per SDK entry point.
extends Node

var _server = BwGodot.new()


func login(user: String, password: String) -> bool:
    return _server.login(user, password)


func logout() -> void:
    _server.logout()


func rpc(entity_id: int, method: String, parameters: Array) -> int:
    return _server.rpc(entity_id, method, parameters)


func get_property(entity_id: int, path: String) -> bool:
    return _server.get_property(entity_id, path)


func set_property(entity_id: int, path: String, value: String) -> bool:
    return _server.set_property(entity_id, path, value)
