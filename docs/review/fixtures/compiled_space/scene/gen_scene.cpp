// Generates a genuine scene.bin using the ORIGINAL BigWorld compiled_space
// writer code (binary_format_writer + string_table_writer + asset_list_writer
// + settings_writer + static_scene_writer + octree_writer +
// static_scene_model_writer), compiled natively on Linux against
// build/cmake-resmgr libs. Section set: BWST + BWAL + BWCS + BWSS + BWSM.
//
// After writing, the container is re-opened with the ORIGINAL readers
// (BinaryFormat::Stream::read overloads) and the provider query chain
// (octree intersect -> span expansion -> sphere filter) is replayed for a
// set of probe regions. The printed object-index sets are the ground truth
// the Rust port must reproduce.
#include <cstdio>
#include <set>
#include <vector>

#include "compiled_space/binary_writers/binary_format_writer.hpp"
#include "compiled_space/binary_writers/string_table_writer.hpp"
#include "compiled_space/binary_writers/asset_list_writer.hpp"
#include "compiled_space/binary_writers/settings_writer.hpp"
#include "compiled_space/binary_writers/static_scene_writer.hpp"
#include "compiled_space/binary_writers/static_scene_model_writer.hpp"

#include "resmgr/bwresource.hpp"
#include "cstdmf/command_line.hpp"

#include "compiled_space/binary_format.hpp"
#include "compiled_space/string_table.hpp"
#include "compiled_space/binary_format_types.hpp"
#include "compiled_space/string_table_types.hpp"
#include "compiled_space/asset_list_types.hpp"
#include "compiled_space/compiled_space_settings_types.hpp"
#include "compiled_space/static_scene_types.hpp"
#include "compiled_space/static_scene_model_types.hpp"

using namespace BW;
using namespace BW::CompiledSpace;

static bool fail( const char* msg )
{
	fprintf( stderr, "HARNESS FAIL: %s\n", msg );
	return false;
}

static bool sphereOverlapsBox( const Sphere& s, const AABB& bb )
{
	Vector3 c = s.center();
	float r = s.radius();
	float dx = std::max( bb.minBounds().x - c.x, 0.f );
	dx = std::max( dx, c.x - bb.maxBounds().x );
	float dy = std::max( bb.minBounds().y - c.y, 0.f );
	dy = std::max( dy, c.y - bb.maxBounds().y );
	float dz = std::max( bb.minBounds().z - c.z, 0.f );
	dz = std::max( dz, c.z - bb.maxBounds().z );
	return (dx*dx + dy*dy + dz*dz) <= r*r;
}

static void printVector3( const char* tag, const Vector3& v )
{
	printf( "%s %.6f %.6f %.6f\n", tag, v.x, v.y, v.z );
}

static void printAABB( const char* tag, const AABB& bb )
{
	printf( "%s %.6f %.6f %.6f %.6f %.6f %.6f\n", tag,
		bb.minBounds().x, bb.minBounds().y, bb.minBounds().z,
		bb.maxBounds().x, bb.maxBounds().y, bb.maxBounds().z );
}

static void probeQuery( const char* tag, const AABB& probe,
	StaticLooseOctree& octree,
	BW::ExternalArray< DataSpan >& spans,
	BW::ExternalArray< uint32 >& contents,
	BW::ExternalArray< Sphere >& spheres,
	BW::ExternalArray< StaticSceneTypes::TypeHeader >& typeHeaders )
{
	StaticLooseOctree::RuntimeNodeDataReferences refs;
	octree.intersect( probe, refs );

	std::set< uint32 > hits;
	size_t nodeHits = 0;
	for (size_t i = 0; i < refs.size(); ++i)
	{
		++nodeHits;
		const DataSpan& span = spans[ refs[i] ];
		for (uint32 j = span.first(); j <= span.last(); ++j)
		{
			uint32 objIdx = contents[ j ];
			if (sphereOverlapsBox( spheres[ objIdx ], probe ))
			{
				hits.insert( objIdx );
			}
		}
	}

	printf( "PROBE %s nodes=%d objects=", tag, (int)nodeHits );
	for (std::set< uint32 >::iterator it = hits.begin();
		it != hits.end(); ++it)
	{
		printf( "%d,", (int)*it );
	}
	printf( "0xDEAD\n" );

	// For each hit also print the owning type header (all models here).
	if (!hits.empty())
	{
		printf( "PROBE_TYPES %s", tag );
		for (std::set< uint32 >::iterator it = hits.begin();
			it != hits.end(); ++it)
		{
			uint32 acc = 0;
			uint32 ownerType = 0xFFFFFFFF;
			for (size_t t = 0; t < typeHeaders.size(); ++t)
			{
				if (*it < acc + typeHeaders[t].numObjects_)
				{
					ownerType = typeHeaders[t].typeID_;
					break;
				}
				acc += typeHeaders[t].numObjects_;
			}
			printf( " %d:%d", (int)*it, (int)ownerType );
		}
		printf( "\n" );
	}
}

int main( int argc, char* argv[] )
{
	const char* outPath = (argc > 1) ? argv[1] : "out/scene.bin";

	int resArgc = 1;
	BWResource resInstance;
	BWResource::init( resArgc, NULL );

	printf( "sizeof(Model)=%zu TypeHeader=%zu Configuration=%zu Node=%zu "
		"NodeBoundData=%zu Sphere=%zu DataSpan=%zu\n",
		sizeof( StaticSceneModelTypes::Model ),
		sizeof( StaticSceneTypes::TypeHeader ),
		sizeof( LooseOctree_DataTypes::Configuration ),
		sizeof( LooseOctree_DataTypes::Node ),
		sizeof( LooseOctree_DataTypes::NodeBoundData ),
		sizeof( BW::Sphere ),
		sizeof( DataSpan ) );

	StringTableWriter strings;
	AssetListWriter assets;
	SettingsWriter settings;

	// The model writer itself adds its resources to the asset list; add a
	// couple of extra unrelated entries so the asset list is not exactly
	// the model resource set (mirrors real spaces with textures/effects).
	assets.addAsset( AssetListTypes::ASSET_TYPE_TEXTURE,
		"spaces/sample_set/textures/grass.tga", strings );
	assets.addAsset( AssetListTypes::ASSET_TYPE_EFFECT,
		"shaders/std_effects/light_only.fx", strings );
	strings.addString( "spaces/sample_set/chunks/001.chunk" );

	settings.addBounds( AABB( Vector3( -50, -10, -50 ), Vector3( 50, 30, 50 ) ) );
	settings.postProcess();

	StaticSceneModelWriter modelWriter;
	StaticSceneWriter sceneWriter( StaticSceneTypes::FORMAT_MAGIC[
		StaticSceneTypes::eDEFAULT ] );
	// Mirror the space_converter wiring: the base ISpaceWriter gets the
	// shared string/asset writers before handlers are configured.
	sceneWriter.configure( &strings, &assets );
	sceneWriter.addType( &modelWriter );

	CommandLine cmdLine( "" );
	if (!sceneWriter.initialize( DataSectionPtr(), cmdLine ))
	{
		return fail( "scene writer init" ) ? 1 : 1;
	}

	// Feed the five fixture model items through the original chunk-model
	// conversion path. chunkTransform is identity; the item transform
	// carries placement/scale.
	const char* items[] = { "models/m01.xml", "models/m02.xml",
		"models/m03.xml", "models/m04.xml", "models/m05.xml" };
	Matrix identity; identity.setIdentity();
	for (size_t i = 0; i < 5; ++i)
	{
		DataSectionPtr pDS = BWResource::instance().openSection( items[i] );
		if (!pDS)
		{
			return fail( items[i] ) ? 1 : 1;
		}
		if (!modelWriter.addFromChunkModel( pDS, identity, false,
			strings, assets ))
		{
			return fail( "addFromChunkModel" ) ? 1 : 1;
		}
	}

	BinaryFormatWriter writer;
	if (!strings.write( writer )) return fail( "strings" ) ? 1 : 1;
	if (!assets.write( writer )) return fail( "assets" ) ? 1 : 1;
	if (!settings.write( writer )) return fail( "settings" ) ? 1 : 1;
	if (!sceneWriter.write( writer )) return fail( "scene" ) ? 1 : 1;
	if (!writer.write( outPath )) return fail( "container write" ) ? 1 : 1;

	printf( "wrote %s: sections=%d\n", outPath, (int)writer.numSections() );

	// ---- read back with the ORIGINAL readers and replay provider queries.
	// The Linux FileHandleStreamer cannot memory-map files
	// (CRITICAL "Unimplemented"), so BinaryFormat::open cannot be used here.
	// Instead the container is unwrapped by hand using the ORIGINAL
	// Header/SectionHeader layouts (byte layout pinned in BW-10-02-01) and
	// the payloads are fed to the ORIGINAL stream readers through the
	// public BinaryFormat::Stream(buffer, length) constructor.
	FILE* f = fopen( outPath, "rb" );
	if (!f)
	{
		return fail( "reopen" ) ? 1 : 1;
	}
	fseek( f, 0, SEEK_END );
	size_t fileLen = ftell( f );
	fseek( f, 0, SEEK_SET );
	std::vector< char > fileBytes( fileLen );
	if (fread( &fileBytes.front(), 1, fileLen, f ) != fileLen)
	{
		fclose( f );
		return fail( "reread" ) ? 1 : 1;
	}
	fclose( f );

	typedef BinaryFormatTypes::Header Header;
	typedef BinaryFormatTypes::SectionHeader SectionHeader;
	Header* pHeader = (Header*)&fileBytes.front();
	SectionHeader* pSections = (SectionHeader*)( &fileBytes.front() +
		sizeof( Header ) );

	size_t bwssIdx = BinaryFormat::NOT_FOUND;
	size_t bwsmIdx = BinaryFormat::NOT_FOUND;
	for (uint32 i = 0; i < pHeader->numSections_; ++i)
	{
		printf( "SECTION %u %.4s v%u len %d\n", i,
			(const char*)&pSections[i].magic_,
			pSections[i].version_,
			(int)pSections[i].length_ );
		if (pSections[i].magic_ == StaticSceneTypes::FORMAT_MAGIC[
			StaticSceneTypes::eDEFAULT ])
		{
			bwssIdx = i;
		}
		else if (pSections[i].magic_ == StaticSceneModelTypes::FORMAT_MAGIC )
		{
			bwsmIdx = i;
		}
	}
	if (bwssIdx == BinaryFormat::NOT_FOUND ||
		bwsmIdx == BinaryFormat::NOT_FOUND)
	{
		return fail( "section lookup" ) ? 1 : 1;
	}

	// string table dump (original reader)
	BinaryFormat::Stream stStream(
		&fileBytes.front() + pSections[0].offset_, (size_t)pSections[0].length_ );
	StringTable table;
	if (!table.readFromStream( &stStream ))
	{
		return fail( "string table read" ) ? 1 : 1;
	}
	for (size_t i = 0; i < table.size(); ++i)
	{
		printf( "STR %d %s\n", (int)i, table.entry( i ) );
	}

	// BWSS section (original readers)
	printf( "BWSS v%u\n", pSections[bwssIdx].version_ );

	BinaryFormat::Stream stream(
		&fileBytes.front() + pSections[bwssIdx].offset_,
		(size_t)pSections[bwssIdx].length_ );

	StaticLooseOctree octree;
	BW::ExternalArray< DataSpan > spans;
	BW::ExternalArray< uint32 > contents;
	BW::ExternalArray< AABB > bounds;
	BW::ExternalArray< Sphere > spheres;
	BW::ExternalArray< StaticSceneTypes::TypeHeader > typeHeaders;
	if (!stream.read( octree ) ||
		!stream.read( spans ) ||
		!stream.read( contents ) ||
		!stream.read( bounds ) ||
		!stream.read( spheres ))
	{
		return fail( "BWSS spatial read" ) ? 1 : 1;
	}
	if (!stream.read( typeHeaders ))
	{
		return fail( "type headers read" ) ? 1 : 1;
	}

	printf( "OCTREE maxDepth %u numDataEntries %u\n",
		octree.storage().config_.maxDepth_,
		octree.storage().config_.numDataEntries_ );
	printVector3( "OCTREE_CENTER", octree.storage().config_.worldCenter_ );
	printf( "OCTREE_WORLDSIZE %.6f\n",
		octree.storage().config_.worldSize_ );
	printf( "OCTREE_ARRAYS nodes %d bounds %d centers %d dataRefs %d "
		"parents %d\n",
		(int)octree.storage().nodes_.size(),
		(int)octree.storage().bounds_.size(),
		(int)octree.storage().centers_.size(),
		(int)octree.storage().dataReferences_.size(),
		(int)octree.storage().parents_.size() );
	for (size_t i = 0; i < octree.storage().nodes_.size(); ++i)
	{
		const LooseOctree_DataTypes::Node& n =
			octree.storage().nodes_[ i ];
		printAABB( "NODEBB", octree.storage().bounds_[ i ].box );
		printVector3( "NODECENTER", octree.storage().centers_[ i ] );
		printf( "NODE dataRef %u parent %u children",
			(int)octree.storage().dataReferences_[ i ],
			(int)octree.storage().parents_[ i ] );
		for (uint32 c = 0; c < 8; ++c)
		{
			printf( " %u", (int)n.child[ c ] );
		}
		printf( "\n" );
	}
	for (size_t i = 0; i < spans.size(); ++i)
	{
		printf( "SPAN %d %u %u\n", (int)i, spans[i].first(),
			spans[i].last() );
	}
	for (size_t i = 0; i < contents.size(); ++i)
	{
		printf( "CONTENT %d %u\n", (int)i, contents[ i ] );
	}
	for (size_t i = 0; i < typeHeaders.size(); ++i)
	{
		printf( "TYPEHDR %d typeID %u numObjects %u\n", (int)i,
			(int)typeHeaders[i].typeID_,
			typeHeaders[i].numObjects_ );
	}
	for (size_t i = 0; i < bounds.size(); ++i)
	{
		printAABB( "OBJBB", bounds[ i ] );
		printf( "OBJSPHERE %.6f %.6f %.6f %.6f\n",
			spheres[i].center().x, spheres[i].center().y,
			spheres[i].center().z, spheres[i].radius() );
	}

	// BWSM section (original readers)
	printf( "BWSM v%u\n", pSections[bwsmIdx].version_ );

	BinaryFormat::Stream mstream(
		&fileBytes.front() + pSections[bwsmIdx].offset_,
		(size_t)pSections[bwsmIdx].length_ );

	BW::ExternalArray< StaticSceneModelTypes::Model > models;
	BW::ExternalArray< StringTableTypes::Index > resourceRefs;
	if (!mstream.read( models ) || !mstream.read( resourceRefs ))
	{
		return fail( "BWSM data read" ) ? 1 : 1;
	}
	for (size_t i = 0; i < models.size(); ++i)
	{
		const StaticSceneModelTypes::Model& m = models[ i ];
		printf( "MODEL %d resRange %u..%u animName %u mult %.6f flags %u "
			"dyeRange %u..%u\n",
			(int)i, m.resourceIDs_.first_, m.resourceIDs_.last_,
			m.animationName_, m.animationMultiplier_, m.flags_,
			m.tintDyes_.first_, m.tintDyes_.last_ );
		for (uint32 r = 0; r < 4; ++r)
		{
			printf( "MX %d %d %.6f %.6f %.6f %.6f\n", (int)i, (int)r,
				m.worldTransform_.m[ r ][ 0 ],
				m.worldTransform_.m[ r ][ 1 ],
				m.worldTransform_.m[ r ][ 2 ],
				m.worldTransform_.m[ r ][ 3 ] );
		}
	}
	for (size_t i = 0; i < resourceRefs.size(); ++i)
	{
		printf( "RESREF %d %u %s\n", (int)i, resourceRefs[ i ],
			table.entry( resourceRefs[ i ] ) );
	}

	// Provider query replay over probe regions (object indices are
	// scene indices = per-type indices accumulated across type headers;
	// here there is exactly one type so they match model indices).
	{
		AABB whole( Vector3( -45, -2, -45 ), Vector3( 42, 10, 45 ) );
		probeQuery( "whole", whole, octree, spans, contents, spheres,
			typeHeaders );
	}
	{
		AABB m1( Vector3( 5, 3, 5 ), Vector3( 15, 7, 15 ) );
		probeQuery( "m01", m1, octree, spans, contents, spheres,
			typeHeaders );
	}
	{
		AABB m2( Vector3( -45, 0, -45 ), Vector3( -37, 4, -37 ) );
		probeQuery( "m02", m2, octree, spans, contents, spheres,
			typeHeaders );
	}
	{
		AABB m3( Vector3( 38, 6, -42 ), Vector3( 42, 10, -38 ) );
		probeQuery( "m03", m3, octree, spans, contents, spheres,
			typeHeaders );
	}
	{
		AABB m4( Vector3( -45, -2, 35 ), Vector3( -37, 8, 45 ) );
		probeQuery( "m04", m4, octree, spans, contents, spheres,
			typeHeaders );
	}
	{
		AABB m5( Vector3( 38, 4, 38 ), Vector3( 42, 8, 42 ) );
		probeQuery( "m05", m5, octree, spans, contents, spheres,
			typeHeaders );
	}
	{
		AABB empty( Vector3( 0, 0, 0 ), Vector3( 2, 2, 2 ) );
		probeQuery( "empty", empty, octree, spans, contents, spheres,
			typeHeaders );
	}
	{
		AABB two( Vector3( -45, 0, -45 ), Vector3( 42, 10, -38 ) );
		probeQuery( "two", two, octree, spans, contents, spheres,
			typeHeaders );
	}

	return 0;
}
