// Generates a genuine space.bin using the ORIGINAL BigWorld compiled_space
// writer code (binary_format_writer + string_table_writer + asset_list_writer
// + settings_writer), compiled natively on Linux against build/cmake-resmgr
// libs. Section set mirrors what tools/asset_pipeline space_converter emits
// for a plain space (BWST + BWAL + BWCS).
#include <cstdio>

#include "compiled_space/binary_writers/binary_format_writer.hpp"
#include "compiled_space/binary_writers/string_table_writer.hpp"
#include "compiled_space/binary_writers/asset_list_writer.hpp"
#include "compiled_space/binary_writers/settings_writer.hpp"

#include "resmgr/bwresource.hpp"

#include "compiled_space/binary_format_types.hpp"
#include "compiled_space/string_table_types.hpp"
#include "compiled_space/asset_list_types.hpp"
#include "compiled_space/compiled_space_settings_types.hpp"

using namespace BW;
using namespace BW::CompiledSpace;

int main( int argc, char* argv[] )
{
	const char* outPath = (argc > 1) ? argv[1] : "space.bin";

	int resArgc = 1;
	BWResource resInstance;
	BWResource::init( resArgc, NULL );

	printf( "sizeof(Header)=%zu SectionHeader=%zu ST.Entry=%zu AL.Entry=%zu CS.Header=%zu\n",
		sizeof( BinaryFormatTypes::Header ),
		sizeof( BinaryFormatTypes::SectionHeader ),
		sizeof( StringTableTypes::Entry ),
		sizeof( AssetListTypes::Entry ),
		sizeof( CompiledSpaceSettingsTypes::Header ) );

	StringTableWriter strings;
	AssetListWriter assets;
	SettingsWriter settings;

	assets.addAsset( AssetListTypes::ASSET_TYPE_MODEL,
		"spaces/sample_set/spaces/01.model", strings );
	assets.addAsset( AssetListTypes::ASSET_TYPE_TEXTURE,
		"spaces/sample_set/textures/grass.tga", strings );
	assets.addAsset( AssetListTypes::ASSET_TYPE_EFFECT,
		"shaders/std_effects/light_only.fx", strings );
	// duplicate asset path: exercises the writer's dedupe-by-stringIndex
	assets.addAsset( AssetListTypes::ASSET_TYPE_MODEL,
		"spaces/sample_set/spaces/01.model", strings );
	// an extra string not referenced by the asset list (chunk path)
	strings.addString( "spaces/sample_set/chunks/001.chunk" );

	settings.addBounds( AABB( Vector3( -50, -10, -50 ), Vector3( 50, 30, 50 ) ) );
	settings.postProcess();

	BinaryFormatWriter writer;
	if (!strings.write( writer )) return 1;
	if (!assets.write( writer )) return 1;
	if (!settings.write( writer )) return 1;
	if (!writer.write( outPath )) return 1;

	printf( "wrote %s: sections=%d\n", outPath, (int)writer.numSections() );
	return 0;
}
