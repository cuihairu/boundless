// Harness-only compile shim: static_scene_writer.cpp relies on an unqualified
// max() that the original build pulled in transitively. Sources untouched.
#include <algorithm>
using std::max;
using std::min;
