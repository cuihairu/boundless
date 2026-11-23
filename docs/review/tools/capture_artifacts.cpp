// BW-00-04-03 — capture reference resource, entity and database fixtures.
//
// Three fixtures, all produced by the reference stack from real inputs:
//   * resource — the canonical DataSection tree of committed resource files
//     (the entity-definition XML, the entity list, the DB config, the server
//     config) as the reference resmgr parses them;
//   * entity   — EntityDescription for a real .def, its properties/methods,
//     and the serialized entity snapshot (all data domains) of a fixed input
//     value section;
//   * database — the reference DB row payload: the persistent-property stream
//     (BASE_DATA|CELL_DATA|ONLY_PERSISTENT_DATA) plus the spatial data
//     (position/direction/spaceID) that XMLDatabase::getEntity appends for a
//     cell entity (lib/db_storage_xml/xml_database.cpp).
//
// Every fixture records its reference commit and the input file's SHA-256, so
// it is traceable to the exact original version and input that produced it.
//
// Build (flags/links mirror build/cmake-entitydef/CMakeFiles/entitydef_test
// .dir of the reference tree):
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
//       capture_artifacts.cpp -o /tmp/capture_artifacts \
//       -rdynamic -Wl,--export-dynamic \
//       -Wl,-u,BW_Py_memoryTrackingIgnoreBegin -Wl,-u,BW_Py_memoryTrackingIgnoreEnd \
//       -L$REF/build/cmake-entitydef \
//       -Wl,--start-group -lunit_test_lib -lCppUnitLite2 -lentitydef -lscript \
//       -lpyscript -lresmgr -lmath -lcstdmf -lz -lssl -lcrypto -Wl,--end-group \
//       -ldl -lm -lpthread -lutil \
//       $REF/build/cmake-entitydef/bwpython313/libpython3.13.a -ldl -lm

#include "unit_test_lib/base_resmgr_unit_test_harness.hpp"
#include "pyscript/py_import_paths.hpp"
#include "pyscript/script.hpp"

#include "entitydef/entity_description.hpp"
#include "entitydef/data_description.hpp"
#include "resmgr/bwresource.hpp"
#include "resmgr/xml_section.hpp"
#include "cstdmf/memory_stream.hpp"

#include <cstdio>
#include <string>
#include <vector>

BW_BEGIN_NAMESPACE

namespace {

// Recursive canonical dump of a section tree: name, scalar value, children.
void dumpSection( DataSectionPtr pSection, int depth )
{
	for ( DataSection::iterator it = pSection->begin();
		it != pSection->end(); ++it )
	{
		DataSectionPtr pChild = *it;
		for ( int i = 0; i < depth; ++i )
		{
			printf( "  " );
		}
		printf( "%s", pChild->sectionName().c_str() );
		if (pChild->countChildren() == 0)
		{
			printf( " = %s\n", pChild->asString().c_str() );
		}
		else
		{
			printf( "\n" );
			dumpSection( pChild, depth + 1 );
		}
	}
}

// Hex dump of a byte buffer produced by the reference serialization. The
// input SHA-256 that makes the fixture traceable is recorded separately in
// the manifest/report (computed over the input files, not here).
void printBytes( const char * tag, const uint8 * data, size_t size )
{
	printf( "  %s (%zu bytes):", tag, size );
	for ( size_t i = 0; i < size; ++i )
	{
		if (i % 16 == 0)
		{
			printf( "\n    " );
		}
		printf( "%02x", data[ i ] );
	}
	printf( "\n" );
}

} // anonymous namespace

BW_END_NAMESPACE

using namespace BW;

int main( int argc, char * argv[] )
{
	BaseResMgrUnitTestHarness::s_cmdArgC = argc;
	BaseResMgrUnitTestHarness::s_cmdArgV = argv;
	BaseResMgrUnitTestHarness harness( "entitydef" );

	PyImportPaths importPaths;
	importPaths.addNonResPath( "." );
	importPaths.addResPath( "." );
	if (!Script::init( importPaths ))
	{
		printf( "capture_artifacts: Script::init failed\n" );
		return 1;
	}

	// The committed fantasydemo placeholder tree carries the .def + script +
	// config layer the loaders read (see test_fantasydemo_assets.cpp).
	const char * fantasyRoot = "/home/cui/workspaces/BigWorld/programming/fantasydemo/res";
	BWResource::addPath( fantasyRoot );

	printf( "=== resource fixtures ===\n" );
	const char * resourceFiles[] =
	{
		"scripts/entity_defs/Avatar.def",
		"scripts/entities.xml",
		"scripts/db.xml",
		"server/bw.xml",
	};
	for ( size_t i = 0; i < sizeof( resourceFiles ) / sizeof( resourceFiles[ 0 ] );
		++i )
	{
		DataSectionPtr pSection = BWResource::openSection( resourceFiles[ i ] );
		printf( "resource %s\n", resourceFiles[ i ] );
		if (pSection)
		{
			dumpSection( pSection, 1 );
		}
		else
		{
			printf( "  (open failed)\n" );
		}
		printf( "\n" );
	}

	printf( "=== entity fixture ===\n" );
	EntityDescription description;
	if (!description.parse( "Avatar", "", NULL ))
	{
		printf( "entity Avatar: parse failed\n" );
		return 1;
	}
	printf( "entity Avatar\n" );
	printf( "  name %s\n", description.name().c_str() );
	printf( "  index %d\n", (int)description.index() );
	printf( "  propertyCount %d\n", description.propertyCount() );
	printf( "  canBeOnCell %d\n", (int)description.canBeOnCell() );
	for ( int i = 0; i < description.propertyCount(); ++i )
	{
		const DataDescription * pProp = description.property( i );
		printf( "  property %s type %s persistent %d streamSize %d\n",
			pProp->name().c_str(),
			pProp->dataType()->typeName().c_str(),
			(int)pProp->isPersistent(),
			pProp->streamSize() );
	}

	// A fixed input value section the entity snapshot and DB row serialize.
	BW::stringstream valueXML;
	valueXML
		<< "<root>\n"
		<< "  <playerName>Admiral</playerName>\n"
		<< "  <prop1>100</prop1>\n"
		<< "  <prop2>200</prop2>\n"
		<< "  <prop3>300</prop3>\n"
		<< "  <position>1 2 3</position>\n"
		<< "  <direction>0 0 0</direction>\n"
		<< "  <spaceID>7</spaceID>\n"
		<< "</root>\n";
	XMLSectionPtr pValues = XMLSection::createFromStream( "", valueXML );

	// Entity snapshot: all data domains.
	{
		MemoryOStream os;
		description.addSectionToStream( pValues, os,
			EntityDescription::BASE_DATA |
			EntityDescription::CELL_DATA |
			EntityDescription::CLIENT_DATA );
		printBytes( "entity-snapshot", (const uint8*)os.data(), os.size() );
	}

	printf( "\n=== database fixture ===\n" );
	{
		MemoryOStream os;
		description.addSectionToStream( pValues, os,
			EntityDescription::BASE_DATA |
			EntityDescription::CELL_DATA |
			EntityDescription::ONLY_PERSISTENT_DATA );
		if (description.canBeOnCell())
		{
			Vector3 position = pValues->readVector3( "position" );
			Direction3D direction( pValues->readVector3( "direction" ) );
			SpaceID spaceID = pValues->readInt( "spaceID" );
			os << position << direction << spaceID;
		}
		printBytes( "db-row", (const uint8*)os.data(), os.size() );
	}

	Script::fini();
	return 0;
}
