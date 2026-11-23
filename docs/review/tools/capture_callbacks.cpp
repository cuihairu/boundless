// BW-00-04-02 — capture reference Python-callback sequences.
//
// This program drives the reference stack's own Python callback machinery
// (ScriptEventList / ScriptEvents in lib/pyscript) and prints the exact
// sequence in which registered Python listeners fire, the argument each one
// receives, and the return value each produces. Every listener is a real
// Python callable executed by the reference's embedded Python 3.13, so the
// captured order/args/results are the original stack's behaviour, not a
// reimplementation.
//
// The capture covers the callback invariants the differential harness needs:
//   * order     — listeners fire in level order, stable for equal levels;
//   * params    — each listener sees exactly the argument passed to trigger;
//   * ignorable — fields that legitimately vary between runs (here a
//                 timestamp argument) are marked IGNORABLE in the manifest.
//
// Build (flags/links mirror build/cmake-entitydef/CMakeFiles/entitydef_test
// .dir of the reference tree; the harness comes from libunit_test_lib.a):
//
//   REF=<reference checkout>/programming/bigworld
//   g++ -O2 -DNDEBUG -std=gnu++23 -fPIE -m64 -pipe -Wno-deprecated \
//       -Wno-uninitialized -Wno-char-subscripts -Wno-non-virtual-dtor \
//       -Wno-write-strings -Wno-unused-parameter \
//       -DBW_BUILD_PLATFORM='"el7"' -DCODE_INLINE -DMF_CONFIG='"hybrid"' \
//       -DMF_SERVER -DMF_USE_ASSERTS -DUSE_OPENSSL -D_HYBRID \
//       -I$REF/lib -I$REF/third_party -I$REF/third_party/CppUnitLite2/src \
//       -isystem $REF/build/cmake-entitydef/vcpkg_installed/x64-linux-bw/include \
//       -isystem $REF/third_party/python/Include \
//       -isystem $REF/build/cmake-entitydef/bwpython313 \
//       capture_callbacks.cpp -o /tmp/capture_callbacks \
//       -L$REF/build/cmake-entitydef \
//       -Wl,--start-group -lunit_test_lib -lCppUnitLite2 -lscript -lpyscript \
//       -lresmgr -lmath -lcstdmf libz.a libssl.a libcrypto.a -Wl,--end-group \
//       -ldl -lm -lpthread -lutil \
//       $REF/build/cmake-entitydef/bwpython313/libpython3.13.a -ldl -lm

#include "unit_test_lib/base_resmgr_unit_test_harness.hpp"
#include "pyscript/py_import_paths.hpp"
#include "pyscript/script.hpp"
#include "pyscript/script_events.hpp"

#include <cstdio>
#include <string>
#include <vector>

BW_BEGIN_NAMESPACE

namespace {

// Evaluates Python source in the __main__ module namespace (same helper the
// reference's own pyscript unit tests use).
bool runPython( const char * source )
{
	PyObject * pMain = PyImport_AddModule( "__main__" );
	PyObject * pGlobals = PyModule_GetDict( pMain );
	PyObject * pResult = PyRun_String( source, Py_file_input,
		pGlobals, pGlobals );
	if (pResult == NULL)
	{
		PyErr_Print();
		return false;
	}
	Py_DECREF( pResult );
	return true;
}

// Fetches an attribute of the __main__ module as a new reference.
PyObject * mainAttr( const char * name )
{
	PyObject * pMain = PyImport_AddModule( "__main__" );
	return PyObject_GetAttrString( pMain, name );
}

// Canonical repr of a Python object; "None" for the null pointer.
std::string pyRepr( PyObject * obj )
{
	if (obj == NULL)
	{
		return "None";
	}
	PyObject * pRepr = PyObject_Repr( obj );
	std::string result;
	if (pRepr != NULL)
	{
		result = PyUnicode_AsUTF8( pRepr );
		Py_DECREF( pRepr );
	}
	else
	{
		PyErr_Clear();
		result = "<repr-error>";
	}
	return result;
}

// Reads __main__.order (list of listener names in fire order) and
// __main__.args (list of the corresponding argument objects).
void fetchTrace( std::vector< std::string > & names,
	std::vector< std::string > & argReprs )
{
	names.clear();
	argReprs.clear();

	PyObjectPtr pOrder( mainAttr( "order" ), PyObjectPtr::STEAL_REFERENCE );
	PyObjectPtr pArgs( mainAttr( "args" ), PyObjectPtr::STEAL_REFERENCE );
	if (pOrder.get() == NULL || pArgs.get() == NULL)
	{
		PyErr_Clear();
		return;
	}

	Py_ssize_t n = PyList_Size( pOrder.get() );
	for (Py_ssize_t i = 0; i < n; ++i)
	{
		PyObject * pName = PyList_GetItem( pOrder.get(), i );
		PyObject * pArg = PyList_GetItem( pArgs.get(), i );
		names.push_back( PyUnicode_AsUTF8( pName ) );
		argReprs.push_back( pyRepr( pArg ) );
	}
}

// Clears the trace before a trigger so the read is unambiguous.
void clearTrace()
{
	PyRun_SimpleString( "order = []\nargs = []\n" );
	PyErr_Clear();
}

// Prints one captured event: the fire order, per-listener args and the
// results list, all as the reference stack produced them.
void emitEvent( const char * name, const char * argTag,
	const std::vector< std::string > & names,
	const std::vector< std::string > & argReprs,
	PyObject * pResults )
{
	printf( "event %s\n", name );
	printf( "  arg %s\n", argTag );
	printf( "  direction cpp->python\n" );
	printf( "  order" );
	for ( const std::string & n : names )
	{
		printf( " %s", n.c_str() );
	}
	printf( "\n" );
	printf( "  args" );
	for ( const std::string & a : argReprs )
	{
		printf( " %s", a.c_str() );
	}
	printf( "\n" );
	printf( "  results" );
	if (pResults != NULL)
	{
		Py_ssize_t n = PyList_Size( pResults );
		for (Py_ssize_t i = 0; i < n; ++i)
		{
			printf( " %s",
				pyRepr( PyList_GetItem( pResults, i ) ).c_str() );
		}
	}
	printf( "\n\n" );
}

} // anonymous namespace

BW_END_NAMESPACE

using namespace BW;

int main( int argc, char * argv[] )
{
	// The harness synthesises a res path from argv when --root is given,
	// exactly as the reference unit tests do. Constructing it initialises
	// BWResource, which PyImportPaths::addResPath needs.
	BaseResMgrUnitTestHarness::s_cmdArgC = argc;
	BaseResMgrUnitTestHarness::s_cmdArgV = argv;
	BaseResMgrUnitTestHarness harness( "pyscript" );

	PyImportPaths importPaths;
	importPaths.addNonResPath( "." );
	importPaths.addResPath( "." );
	if (!Script::init( importPaths ))
	{
		printf( "capture_callbacks: Script::init failed\n" );
		return 1;
	}

	// A listener set whose fire order, args and returns we capture. Three
	// listeners: a at level -1, b and c at equal level 0 (stable insertion
	// order). The PyObjectPtr holders and the event list are scoped so they
	// are released before Script::fini tears the interpreter down.
	if (true)
	{
	if (!runPython(
		"order = []\n"
		"args = []\n"
		"def listener_a( arg ):\n"
		"\torder.append( 'a' )\n"
		"\targs.append( arg )\n"
		"\treturn 'A-return'\n"
		"def listener_b( arg ):\n"
		"\torder.append( 'b' )\n"
		"\targs.append( arg )\n"
		"\treturn None\n"
		"def listener_c( arg ):\n"
		"\torder.append( 'c' )\n"
		"\targs.append( arg )\n"
		"\treturn arg\n" ))
	{
		return 1;
	}

	PyObjectPtr pA( mainAttr( "listener_a" ), PyObjectPtr::STEAL_REFERENCE );
	PyObjectPtr pB( mainAttr( "listener_b" ), PyObjectPtr::STEAL_REFERENCE );
	PyObjectPtr pC( mainAttr( "listener_c" ), PyObjectPtr::STEAL_REFERENCE );

	ScriptEventList list;
	list.add( pA.get(), -1 );
	list.add( pB.get(), 0 );
	list.add( pC.get(), 0 );

	// 1. int argument.
	clearTrace();
	{
		ScriptList results( PyList_New( 0 ), ScriptObject::STEAL_REFERENCE );
		list.triggerEvent( Py_BuildValue( "(i)", 42 ), results );
		std::vector< std::string > names, argReprs;
		fetchTrace( names, argReprs );
		emitEvent( "int-arg", "(42,)", names, argReprs, results.get() );
	}

	// 2. string argument.
	clearTrace();
	{
		ScriptList results( PyList_New( 0 ), ScriptObject::STEAL_REFERENCE );
		list.triggerEvent( Py_BuildValue( "(s)", "hello" ), results );
		std::vector< std::string > names, argReprs;
		fetchTrace( names, argReprs );
		emitEvent( "str-arg", "('hello',)", names, argReprs, results.get() );
	}

	// 3. tuple argument.
	clearTrace();
	{
		ScriptList results( PyList_New( 0 ), ScriptObject::STEAL_REFERENCE );
		list.triggerEvent( Py_BuildValue( "((ii i))", 1, 2, 3 ), results );
		std::vector< std::string > names, argReprs;
		fetchTrace( names, argReprs );
		emitEvent( "tuple-arg", "((1, 2, 3),)", names, argReprs, results.get() );
	}

	// 4. dict argument.
	clearTrace();
	{
		ScriptList results( PyList_New( 0 ), ScriptObject::STEAL_REFERENCE );
		list.triggerEvent( Py_BuildValue( "({s:i})", "k", 7 ), results );
		std::vector< std::string > names, argReprs;
		fetchTrace( names, argReprs );
		emitEvent( "dict-arg", "({'k': 7},)", names, argReprs, results.get() );
	}

	// 5. None argument (a tuple containing None).
	clearTrace();
	{
		ScriptList results( PyList_New( 0 ), ScriptObject::STEAL_REFERENCE );
		list.triggerEvent( Py_BuildValue( "(O)", Py_None ), results );
		std::vector< std::string > names, argReprs;
		fetchTrace( names, argReprs );
		emitEvent( "none-arg", "(None,)", names, argReprs, results.get() );
	}

	// 6. Timestamp argument: the value varies per run, so the manifest marks
	// it IGNORABLE (a genuine callback field the harness must not compare).
	clearTrace();
	{
		ScriptList results( PyList_New( 0 ), ScriptObject::STEAL_REFERENCE );
		list.triggerEvent( Py_BuildValue( "({s:f})", "ts",
			(double)0.123456789 ), results );
		std::vector< std::string > names, argReprs;
		fetchTrace( names, argReprs );
		emitEvent( "timestamp-arg-IGNORABLE", "({'ts': <float>},)",
			names, argReprs, results.get() );
	}
	} // scope: release Python handles before Script::fini

	Script::fini();
	return 0;
}
