// The Unity engine surface the binding is checked against (BW-21-03-01).
//
// This stands in for the parts of UnityEngine the binding touches, so the
// binding compiles and its shape is checked on a runner that has no Unity
// Editor.  It is not the engine: loading the component in the editor is a
// separate, recorded gap.

namespace UnityEngine
{
    /// The base class of every Unity component.
    public class MonoBehaviour
    {
    }

    /// The engine's log sinks.
    public static class Debug
    {
        public static void Log(string message)
        {
        }

        public static void LogError(string message)
        {
        }
    }
}
