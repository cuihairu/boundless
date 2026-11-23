// The Godot adapter's GDExtension surface (BW-21-05-01).
//
// The native class exposes the SDK entry points a script can call, and the
// entry symbol is the one the extension descriptor names.  Compiling this
// needs godot-cpp, which a bare runner does not have, so the surface is
// checkable from the declarations and the descriptor's symbol cross-checks
// against this file.

#include <godot_cpp/classes/node.hpp>
#include <godot_cpp/core/class_db.hpp>
#include <godot_cpp/core/instance_class.hpp>
#include <godot_cpp/godot.hpp>
#include <godot_cpp/variant/array.hpp>
#include <godot_cpp/variant/string.hpp>

using namespace godot;

/// The native class a script attaches to; the SDK entry points a script can
/// call are public on it.
class BwGodot : public Node
{
    GDCLASS(BwGodot, Node)

public:
    bool login(const String& user, const String& password);
    void logout();
    int64_t rpc(int64_t entity_id, const String& method, const Array& parameters);
    bool get_property(int64_t entity_id, const String& path);
    bool set_property(int64_t entity_id, const String& path, const String& value);
    bool is_logged_in() const { return logged_in_; }

    /// Raise one entity event on the signal, so a connected node receives
    /// it.
    void emit_entity_event(int64_t entity_id, const String& kind);

    static void _bind_methods();

private:
    bool logged_in_ = false;
};

/// Raise one entity event on the signal.
void BwGodot::emit_entity_event(int64_t entity_id, const String& kind)
{
    emit_signal("entity_event", entity_id, kind);
}

/// Bind the script-callable methods, so a GDScript can log in and issue
/// RPCs against the native class, and declare the signal a node connects
/// to for entity events.
void BwGodot::_bind_methods()
{
    ClassDB::bind_method(D_METHOD("login", "user", "password"), &BwGodot::login);
    ClassDB::bind_method(D_METHOD("logout"), &BwGodot::logout);
    ClassDB::bind_method(D_METHOD("rpc", "entity_id", "method", "parameters"), &BwGodot::rpc);
    ClassDB::bind_method(D_METHOD("get_property", "entity_id", "path"), &BwGodot::get_property);
    ClassDB::bind_method(
        D_METHOD("set_property", "entity_id", "path", "value"), &BwGodot::set_property);
    ADD_SIGNAL(MethodInfo("entity_event",
                          PropertyInfo(Variant::Type::INT, "entity_id"),
                          PropertyInfo(Variant::Type::STRING, "kind")));
}

/// Register the class with the engine once the scene module is up.
void initialize_boundless_sdk(ModuleInitializationLevel p_level)
{
    if (p_level == MODULE_INITIALIZATION_LEVEL_SCENE)
    {
        ClassDB::register_class<BwGodot>();
    }
}

void uninitialize_boundless_sdk(ModuleInitializationLevel p_level) {}

/// The entry symbol the extension descriptor names.
extern "C"
{
    GD_EXPORT bool godot_sdk_entry(GDExtensionInterfaceGetProcAddress p_get_proc_address,
                                   GDExtensionClassLibraryPtr p_library,
                                   GDExtensionInitialization* r_initialization)
    {
        GDExtensionBinding::InitObject init(p_get_proc_address, p_library, r_initialization);
        init.register_initializer(initialize_boundless_sdk);
        init.register_terminator(uninitialize_boundless_sdk);
        init.set_minimum_library_initialization_level(MODULE_INITIALIZATION_LEVEL_SCENE);
        return init.init();
    }
}
