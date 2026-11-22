# The signal target (BW-21-05-03): a node connects to the entity_event
# signal and records each entity event it receives.
extends Node

var _received = []


func connect_to_server(server: Node) -> void:
    server.entity_event.connect(_on_entity_event)


func _on_entity_event(entity_id: int, kind: String) -> void:
    _received.append([entity_id, kind])


func received_count() -> int:
    return _received.size()
