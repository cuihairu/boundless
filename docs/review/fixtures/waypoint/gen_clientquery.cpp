// BW-10-07-04 client query harness: the ORIGINAL client query faces over the
// ×10 fixed map, with the chunks REGISTERED WITH THE SPACE — the wiring
// 07-03's harness deliberately lacked, which is exactly what
// `BigWorld.navigatePathPoints` needs (`findFullPath` resolves both
// endpoints through the space constructor, and answers FFPNONE when the
// chunks were never registered).
//
// Two query faces, straight off the reference:
//   RAY      `BigWorld.findDropPoint` (script_bigworld.cpp:129-159):
//            ndPt = (x, y-100, z), dist = space->collide(inPt, ndPt,
//            fdpt) with a ClosestTriangle, dist < 0 => None,
//            outPt = (x, y-dist, z) plus the recorded triangle.
//   NAV      `BigWorld.navigatePathPoints` (script_bigworld.cpp:1880-1929):
//            navigator.findFullPath(space, src, dst, 500, true, 0.6, path)
//            and the "prepend the raw src when it is not almostEqual to
//            the first point" rule. The fixture sets are girth 0.6 and
//            `ChunkNavigator::find` filters sets by almostEqual(girth),
//            so the probes must ask at 0.6 — the script default 0.5
//            answers nothing.
//
// The ground is a one-quad slab per chunk, built through the original
// BSPTreeTool::buildBSP and saved as the fixture blobs the Rust replay
// loads (clientquery_slatA.bsp / _slatB.bsp); a raised platform slab in
// chunk A sits above the ground so the closest-answer rule has work to do.
//
// The obstacle registration goes through the reference's own
// ChunkModelObstacle::addObstacle — the focussed gate plus the eight
// bounding-box corners into the columns — with a bare ChunkItem whose
// chunk() the obstacle's owning-chunk comparison reads. NOTE
// ChunkObstacle::bb_ is a REFERENCE into the caller's box (the real
// obstacle points at the model's own boundingBox()): the boxes here
// must outlive the obstacles or every sweep clips against dead stack
// bytes (that bug made the chunk-B probe miss for a whole session).
//
// compile (gen_mapload.cpp recipe, plus the two BSP faces):
//   g++ -std=c++14 -O2 -o gen_clientquery gen_clientquery.cpp \
//     -I $BW -I $BW/lib -DCODE_INLINE -DCSTDMF_IMPORT -DMF_CONFIG='"hybrid"' \
//     -DMF_SERVER -DMF_USE_ASSERTS -D_HYBRID -DBW_BUILD_PLATFORM='"el7"' \
//     -L $BW/build/el7/lib \
//     -Wl,--start-group -lwaypoint -lchunk -lscene -lresmgr -lmoo \
//     -lmath -lphysics2 -lterrain -lcstdmf -lpng -lz -Wl,--end-group \
//     -lpthread
// run with cwd holding ./mapA.navmesh, ./mapB.navmesh, ./resources.xml
// (environment/materialKinds + system/terrain2), ./material_kinds.xml
// and ./terrain2_defaults.xml (the Terrain::Manager boot addMapping
// needs); the .bsp fixtures are written next to the truth log.

#include <cmath>
#include <cstdio>
#include <cstdint>
#include <cstring>
#include <fstream>
#include <vector>

#include "cstdmf/memory_stream.hpp"
#include "resmgr/auto_config.hpp"
#include "resmgr/binary_block.hpp"
#include "resmgr/bwresource.hpp"
#include "resmgr/xml_section.hpp"

#include "math/vector2.hpp"
#include "math/vector3.hpp"
#include "math/vector4.hpp"

#include "waypoint/chunk_waypoint_set_data.hpp"
#include "waypoint/chunk_waypoint_set.hpp"
#include "waypoint/chunk_waypoint_set_state.hpp"
#include "waypoint/waypoint_neighbour_iterator.hpp"
#include "waypoint/chunk_waypoint_state.hpp"
#include "waypoint/navloc.hpp"
#include "waypoint/chunk_navigator.hpp"
#include "waypoint/navigator.hpp"

#include "chunk/chunk.hpp"
#include "chunk/chunk_boundary.hpp"
#include "chunk/chunk_item.hpp"
#include "chunk/chunk_model_obstacle.hpp"
#include "chunk/chunk_obstacle.hpp"
#include "chunk/chunk_space.hpp"
#include "chunk/geometry_mapping.hpp"

#include "physics2/bsp.hpp"
#include "physics2/collision_callback.hpp"
#include "physics2/worldtri.hpp"
#include "terrain/manager.hpp"

#include "common/closest_triangle.hpp"

using namespace std;
using namespace BW;

namespace
{

// TerrainSettings::init's readIntSetting reaches
// Terrain::Manager::instance().pTerrain2Defaults(), and the MF_SERVER
// singleton asserts when the instance was never constructed. The
// protected init() runs from the constructor itself (manager.cpp:82),
// so constructing the manager is the whole boot.

std::vector< uint8_t > readFile( const char* path )
{
	FILE* f = fopen( path, "rb" );
	if ( !f )
	{
		fprintf( stderr, "cannot open %s\n", path );
		exit( 1 );
	}
	fseek( f, 0, SEEK_END );
	long size = ftell( f );
	fseek( f, 0, SEEK_SET );
	std::vector< uint8_t > blob( static_cast< size_t >( size ) );
	if ( size > 0 )
	{
		if ( fread( &blob[ 0 ], 1, size_t( size ), f ) != size_t( size ) )
		{
			fprintf( stderr, "short read %s\n", path );
			exit( 1 );
		}
	}
	fclose( f );
	return blob;
}

void writeBlob( const char* path, const BinaryBlock& block )
{
	std::ofstream f( path, std::ios::binary );
	if ( !f )
	{
		fprintf( stderr, "cannot write %s\n", path );
		exit( 1 );
	}
	f.write( reinterpret_cast< const char* >( block.data() ),
		block.len() );
}

// the navmeshFactory loader face (gen_mapload.cpp's), record headers only
std::vector< ChunkWaypointSetData* > loadAsset( const char* name,
	const std::vector< uint8_t >& blob, int& base )
{
	printf( "LOAD %s\n", name );
	std::vector< ChunkWaypointSetData* > sets;
	const uint8_t* ptr = &blob[ 0 ];
	const uint8_t* end = ptr + blob.size();
	while ( ptr < end )
	{
		ChunkWaypointSetData* pSet = new ChunkWaypointSetData( base );
		int32_t version = *(const int32_t*)ptr; ptr += 4;
		float   girth   = *(const float*)ptr;   ptr += 4;
		int32_t numWps  = *(const int32_t*)ptr; ptr += 4;
		int32_t numEdges= *(const int32_t*)ptr; ptr += 4;
		MF_ASSERT( version == 0 );
		pSet->girth( girth );
		printf( "LHDR %s girth %.6f waypoints %d edges %d\n", name, girth,
			numWps, numEdges );
		ptr = (const uint8_t*)pSet->readWaypointSet( (const char*)ptr,
			numWps, numEdges );
		base += int( pSet->waypoints().size() );
		sets.push_back( pSet );
	}
	return sets;
}

// a flat quad at the given height, split down the diagonal, winding for
// a +y normal
void addSlab( RealWTriangleSet& tris, const Vector3& o, float x1, float z1 )
{
	const Vector3& a = o;
	const Vector3 b( x1, o.y, o.z );
	const Vector3 c( x1, o.y, z1 );
	const Vector3 d( o.x, o.y, z1 );
	tris.push_back( WorldTriangle( a, d, b, 0 ) );
	tris.push_back( WorldTriangle( b, d, c, 0 ) );
}

} // anonymous namespace

int main()
{
	BWResource bwResource;
	BWResource::instance().init( BW::vector< BW::string >() );
	// the AutoConfig boot (MaterialKinds "environment/materialKinds",
	// Terrain "system/terrain2") resolves through resources.xml in the
	// cwd — the res path has to name the cwd explicitly (BWResource::init
	// with no paths reads ~/.bwmachined.conf, not the cwd).
	BWResource::addPath( "./" );
	AutoConfig::configureAllFrom( "resources.xml" );
	// the manager's constructor runs the (protected) init itself
	// (manager.cpp:82), which loads MaterialKinds and the terrain2
	// defaults; addMapping then runs TerrainSettings::init, which on
	// the server accepts only version 200 (TERRAIN2) — the
	// space.settings' terrain section names it, and its readIntSetting
	// reaches Terrain::Manager::instance().
	Terrain::Manager terrainManager;
	if ( !terrainManager.isValid() )
	{
		fprintf( stderr, "terrain manager init failed\n" );
		return 2;
	}

	std::vector< ChunkWaypointSetData* > setData[2];
	int base = 1;
	{
		std::vector< uint8_t > a = readFile( "./mapA.navmesh" );
		setData[ 0 ] = loadAsset( "mapA", a, base );
	}
	{
		std::vector< uint8_t > b = readFile( "./mapB.navmesh" );
		setData[ 1 ] = loadAsset( "mapB", b, base );
	}
	MF_ASSERT( setData[ 0 ].size() == 1 );
	MF_ASSERT( setData[ 1 ].size() == 1 );

	// ---- two inside chunks with their real world boxes (gen_mapload) ----
	// The mapping is registered THROUGH the space (addMapping) rather than
	// built directly: only addMapping populates mappings_ and runs
	// recalcGridBounds(), which sizes the server ColumnGrid. Without it
	// column() answers NULL for every point and both query faces go dead.
	ChunkSpacePtr pSpace = new ChunkSpace( 7 );
	XMLSectionPtr pSettings = new XMLSection( "space" );
	{
		DataSectionPtr pBounds = pSettings->openSection( "bounds", true );
		pBounds->openSection( "minX", true )->setInt( 0 );
		pBounds->openSection( "minY", true )->setInt( 0 );
		pBounds->openSection( "maxX", true )->setInt( 0 );
		pBounds->openSection( "maxY", true )->setInt( 0 );
		DataSectionPtr pTerrain = pSettings->openSection( "terrain", true );
		// TerrainSettings::init reads a child section named "version"
		// (terrain_settings.cpp:187); the server branch only accepts
		// TERRAIN2_VERSION (200).
		pTerrain->openSection( "version", true )->setInt( 200 );
	}
	Matrix ident = Matrix::identity;
	GeometryMapping* pMapping =
		pSpace->addMapping( SpaceEntryID(), ( float* )&ident, ".",
			pSettings );

	Chunk* pChunk[2];
	const char* names[2] = { "chunkA", "chunkB" };
	const BoundingBox boxes[2] = {
		BoundingBox( Vector3( 0.f, 0.f, 0.f ),
		             Vector3( 5.f, 15.f, 10.f ) ),
		// a different volume than chunk A's: Column::findChunk keeps
		// the STRICTLY larger hull when two hulls both claim a point
		// (`pChunk->volume() > result->pChunk_->volume()`), so equal
		// volumes would make the first-added hull win wherever the
		// trees' tags overlap
		BoundingBox( Vector3( 5.f, 0.f, 0.f ),
		             Vector3( 10.f, 20.f, 10.f ) )
	};
	for ( int c = 0; c < 2; ++c )
	{
		pChunk[ c ] = new Chunk( names[ c ], pMapping, Matrix::identity,
			boxes[ c ] );
		pChunk[ c ]->load( NULL );
		pChunk[ c ]->localBB( boxes[ c ] );
		pChunk[ c ]->boundingBox( boxes[ c ] );
	}

	// ---- each set on its own chunk's navigator ----
	ChunkWaypointSetPtr pWay[2];
	for ( int c = 0; c < 2; ++c )
	{
		pWay[ c ] = new ChunkWaypointSet(
			ChunkWaypointSetDataPtr( setData[ c ][ 0 ] ) );
		pWay[ c ]->chunk( pChunk[ c ] );
		ChunkNavigator::instance( *pChunk[ c ] ).add( pWay[ c ].get() );
		for ( int pin = 0; pin < 512; ++pin )
		{
			pWay[ c ]->incRef();
		}
	}

	// ---- the permissive portal pair on the seam (gen_mapload) ----
	PlaneEq planeAB( Vector3( 1.f, 0.f, 0.f ), 5.f );
	BW::vector< Vector3 > rectAB;
	rectAB.push_back( Vector3( 4.4f, 9.9f, 0.f ) );
	rectAB.push_back( Vector3( 4.4f, 0.1f, 0.f ) );
	rectAB.push_back( Vector3( 5.6f, 0.1f, 0.f ) );
	rectAB.push_back( Vector3( 5.6f, 9.9f, 0.f ) );

	ChunkBoundary::Portal* portals[2];
	portals[ 0 ] = new ChunkBoundary::Portal( planeAB, Vector3( 0, 1, 0 ),
		rectAB, pMapping, "" );
	portals[ 0 ]->pChunk = pChunk[ 1 ];
	portals[ 1 ] = new ChunkBoundary::Portal( planeAB, Vector3( 0, 1, 0 ),
		rectAB, pMapping, "" );
	portals[ 1 ]->pChunk = pChunk[ 0 ];
	for ( int i = 0; i < 2; ++i )
	{
		portals[ i ]->uAxis = Vector3( 1.f, 0.f, 0.f );
		portals[ i ]->vAxis = Vector3( 0.f, 0.f, 1.f );
		portals[ i ]->origin = Vector3( 0.f, 4.1f, 0.f );
	}
	// Chunk::focus() builds the hull border from bounds_ (the physical
	// edges, filled only by formBoundaries), so a chunk queried through
	// the space needs real convex bounds — six box faces each, outward
	// normals (the HullTree walk answers testPoint on the FRONT side),
	// with the seam face doubling as the portal's bound so the chunk
	// still binds.
	{
		const BoundingBox& box = boxes[ 0 ];
		struct FaceDef { PlaneEq peq; };
		// bounds_ faces face INWARD (front side = chunk interior —
		// chunk.cpp's contains/owns read `distanceTo < -radius` as
		// outside), so every normal points into the box
		FaceDef faces[] = {
			// +x is the seam: this face takes the portal's boundary
			{ PlaneEq( Vector3( -1.f, 0.f, 0.f ), -5.f ) },
			{ PlaneEq( Vector3(  1.f, 0.f, 0.f ), 0.f ) },
			{ PlaneEq( Vector3(  0.f, -1.f, 0.f ), -15.f ) },
			{ PlaneEq( Vector3(  0.f, 1.f, 0.f ), 0.f ) },
			{ PlaneEq( Vector3(  0.f, 0.f, -1.f ), -10.f ) },
			{ PlaneEq( Vector3(  0.f, 0.f, 1.f ), 0.f ) },
		};
		(void)box;
		for ( int f = 0; f < 6; ++f )
		{
			if ( f == 0 )
			{
				// the seam face: the bound portal lives here
				ChunkBoundary* pBound =
					new ChunkBoundary( planeAB, portals[ 0 ] );
				pBound->bindPortal( 0 );
				pChunk[ 0 ]->joints().push_back(
					ChunkBoundaryPtr( pBound ) );
				pChunk[ 0 ]->bounds().push_back(
					ChunkBoundaryPtr( pBound ) );
				continue;
			}
			// the Portal ctor needs a real polygon (its test plane
			// walks the first three points), so cap every face with
			// the reference's four-corner flat shape — the cap rect
			// can cross faces safely, the boundary only reads the
			// plane. An earth portal lands straight in boundPortals_
			// (the ChunkBoundary ctor), so there is no bindPortal move.
			BW::vector< Vector3 > capRect;
			capRect.push_back( Vector3( 0.2f, 0.2f, 0.2f ) );
			capRect.push_back( Vector3( 0.8f, 0.2f, 0.2f ) );
			capRect.push_back( Vector3( 0.8f, 0.2f, 0.8f ) );
			capRect.push_back( Vector3( 0.2f, 0.2f, 0.8f ) );
			ChunkBoundary::Portal* pCap = new ChunkBoundary::Portal(
				faces[ f ].peq, Vector3( 0.f, 1.f, 0.f ),
				capRect, pMapping, "earth" );
			// one boundary shared by both lists (the boundary owns the
			// portal, so a second object would double delete it)
			ChunkBoundary* pBound =
				new ChunkBoundary( faces[ f ].peq, pCap );
			pChunk[ 0 ]->joints().push_back(
				ChunkBoundaryPtr( pBound ) );
			pChunk[ 0 ]->bounds().push_back(
				ChunkBoundaryPtr( pBound ) );
		}
	}
	{
		struct FaceDef { PlaneEq peq; };
		// inward normals again; -x is the seam for chunk B. The seam
		// face sits at x=5 with inward normal (1,0,0), so its plane
		// constant is +5 (front side = x > 5 = chunk B's interior).
		FaceDef faces[] = {
			{ PlaneEq( Vector3(  1.f, 0.f, 0.f ),  5.f ) },
			{ PlaneEq( Vector3( -1.f, 0.f, 0.f ), -10.f ) },
			{ PlaneEq( Vector3(  0.f, -1.f, 0.f ), -20.f ) },
			{ PlaneEq( Vector3(  0.f, 1.f, 0.f ), 0.f ) },
			{ PlaneEq( Vector3(  0.f, 0.f, -1.f ), -10.f ) },
			{ PlaneEq( Vector3(  0.f, 0.f, 1.f ), 0.f ) },
		};
		for ( int f = 0; f < 6; ++f )
		{
			if ( f == 0 )
			{
				// the hull border reads the BOUNDARY plane (chunk.cpp:1548),
				// which must point out of chunk B; the seam portal is
				// mounted on it but keeps its own planeAB for projection
				ChunkBoundary* pBound = new ChunkBoundary( faces[ 0 ].peq,
					portals[ 1 ] );
				pBound->bindPortal( 0 );
				pChunk[ 1 ]->joints().push_back(
					ChunkBoundaryPtr( pBound ) );
				pChunk[ 1 ]->bounds().push_back(
					ChunkBoundaryPtr( pBound ) );
				continue;
			}
			BW::vector< Vector3 > capRect;
			capRect.push_back( Vector3( 0.2f, 0.2f, 0.2f ) );
			capRect.push_back( Vector3( 0.8f, 0.2f, 0.2f ) );
			capRect.push_back( Vector3( 0.8f, 0.2f, 0.8f ) );
			capRect.push_back( Vector3( 0.2f, 0.2f, 0.8f ) );
			ChunkBoundary::Portal* pCap = new ChunkBoundary::Portal(
				faces[ f ].peq, Vector3( 0.f, 1.f, 0.f ),
				capRect, pMapping, "earth" );
			// one boundary shared by both lists (the boundary owns the
			// portal, so a second object would double delete it)
			ChunkBoundary* pBound =
				new ChunkBoundary( faces[ f ].peq, pCap );
			pChunk[ 1 ]->joints().push_back(
				ChunkBoundaryPtr( pBound ) );
			pChunk[ 1 ]->bounds().push_back(
				ChunkBoundaryPtr( pBound ) );
		}
	}
	for ( int c = 0; c < 2; ++c )
	{
		pWay[ c ]->bind();
	}

	// ---- the new part: register the chunks with the space and focus ----
	pSpace->addChunk( pChunk[ 0 ] );
	pSpace->addChunk( pChunk[ 1 ] );
	for ( int c = 0; c < 2; ++c )
	{
		// the space's focus() only walks the blurred list, and these
		// chunks were never bound into one, so focus them directly
		pChunk[ c ]->focus();
	}
	printf( "FOCUS space %d gridsize %.6f\n", int( pSpace->id() ),
		pSpace->gridSize() );
	for ( int c = 0; c < 2; ++c )
	{
		printf( "FOCUSED %s %d\n", names[ c ],
			pChunk[ c ]->focussed() ? 1 : 0 );
	}

	// ---- the collision ground: one slab per chunk, saved as fixtures ----
	RealWTriangleSet slabTris[2];
	addSlab( slabTris[ 0 ], Vector3( 0.f, 0.f, 0.f ), 5.f, 10.f );
	addSlab( slabTris[ 1 ], Vector3( 5.f, 0.f, 0.f ), 10.f, 10.f );
	// the raised platform over chunk A's ground
	RealWTriangleSet platTris;
	addSlab( platTris, Vector3( 1.f, 4.f, 1.f ), 4.f, 4.f );

	BSPTree* slatBSP[2];
	for ( int c = 0; c < 2; ++c )
	{
		slatBSP[ c ] = BSPTreeTool::buildBSP( slabTris[ c ] );
		if ( !slatBSP[ c ] )
		{
			fprintf( stderr, "buildBSP failed for chunk %d\n", c );
			return 3;
		}
		const char* blob[2] = { "clientquery_slatA.bsp",
			"clientquery_slatB.bsp" };
		writeBlob( blob[ c ],
			*BSPTreeTool::saveBSPInMemory( slatBSP[ c ] ).get() );
		printf( "BSP %s triangles %d nodes %d\n", blob[ c ],
			int( slatBSP[ c ]->numTriangles() ),
			int( slatBSP[ c ]->numNodes() ) );
	}
	BSPTree* platBSP = BSPTreeTool::buildBSP( platTris );
	if ( !platBSP )
	{
		fprintf( stderr, "buildBSP failed for the platform\n" );
		return 3;
	}
	writeBlob( "clientquery_platform.bsp",
		*BSPTreeTool::saveBSPInMemory( platBSP ).get() );
	printf( "BSP clientquery_platform.bsp triangles %d nodes %d\n",
		int( platBSP->numTriangles() ), int( platBSP->numNodes() ) );

	// ---- register the slabs through the reference's own path: a bare
	// ChunkItem the obstacle's owning-chunk comparison reads ----
	// ChunkObstacle::bb_ is a REFERENCE into the caller's box (the real
	// obstacle points at the model's own boundingBox()), so these must
	// outlive the obstacles — loop-locals would dangle and every sweep
	// would clip against dead stack bytes.
	BoundingBox obsBBs[3] = {
		BoundingBox( boxes[ 0 ].minBounds(), boxes[ 0 ].maxBounds() ),
		BoundingBox( boxes[ 1 ].minBounds(), boxes[ 1 ].maxBounds() ),
		BoundingBox( Vector3( 1.f, 4.f, 1.f ), Vector3( 4.f, 4.f, 4.f ) )
	};
	for ( int c = 0; c < 2; ++c )
	{
		ChunkItemPtr pItem = new ChunkItem();
		pItem->chunk( pChunk[ c ] );
		ChunkBSPObstacle* pOb = new ChunkBSPObstacle( slatBSP[ c ],
			Matrix::identity, &obsBBs[ c ], pItem );
		ChunkModelObstacle::instance( *pChunk[ c ] ).addObstacle( pOb );
	}
	{
		ChunkItemPtr pItem = new ChunkItem();
		pItem->chunk( pChunk[ 0 ] );
		ChunkBSPObstacle* pOb = new ChunkBSPObstacle( platBSP,
			Matrix::identity, &obsBBs[ 2 ], pItem );
		ChunkModelObstacle::instance( *pChunk[ 0 ] ).addObstacle( pOb );
	}

	// ---- the chunk the NavLoc face resolves each probe through. Both
	// points answer chunkA: the seam plane is coplanar in the column's
	// hull tree, so the volume gate (`pChunk->volume() >
	// result->pChunk_->volume()`) flips every contested point to the
	// strictly larger hull — chunk B's box is bigger, so findChunk walks
	// its tag last and chunk A keeps the answer. This is the reference's
	// own resolution for this fixture and the replay tests mirror it.
	{
		Chunk* r6 = pSpace->findChunkFromPointExact(
			Vector3( 6.f, 4.01f, 2.f ) );
		Chunk* r1 = pSpace->findChunkFromPointExact(
			Vector3( 1.f, 4.01f, 2.f ) );
		printf( "CHUNKRES B@6 %d A@1 %d (r6 %s, r1 %s)\n",
			(r6 == pChunk[ 1 ]) ? 1 : 0,
			(r1 == pChunk[ 0 ]) ? 1 : 0,
			r6 ? r6->identifier().c_str() : "NULL",
			r1 ? r1->identifier().c_str() : "NULL" );
	}

	// ---- RAY: BigWorld.findDropPoint over the space ----
	struct RayProbe
	{
		const char* name;
		Vector3     inPt;
	};
	const RayProbe rayProbes[] = {
		{ "GA",     Vector3( 1.f, 5.f, 1.f ) },
		{ "GHOLED", Vector3( 2.5f, 4.f, 1.f ) },
		{ "GB",     Vector3( 6.5f, 7.f, 2.5f ) },
		{ "GPLAT",  Vector3( 2.f, 6.f, 2.f ) },
		{ "GNONE",  Vector3( 50.f, 5.f, 50.f ) },
		{ "GSEAM",  Vector3( 5.f, 3.f, 3.f ) },
	};
	for ( size_t p = 0; p < sizeof( rayProbes ) / sizeof( rayProbes[ 0 ] );
		++p )
	{
		const Vector3& inPt = rayProbes[ p ].inPt;
		ClosestTriangle fdpt;
		const Vector3 ndPt( inPt.x, inPt.y - 100.f, inPt.z );
		const float dist = pSpace->collide( inPt, ndPt, fdpt );
		if ( dist < 0.f )
		{
			printf( "RAYMISS %s %.6f %.6f %.6f\n", rayProbes[ p ].name,
				inPt.x, inPt.y, inPt.z );
			continue;
		}
		const Vector3 outPt( inPt.x, inPt.y - dist, inPt.z );
		const WorldTriangle& t = fdpt.triangle();
		printf( "RAY %s in %.6f %.6f %.6f dist %.6f out %.6f %.6f %.6f "
			"tri %.6f %.6f %.6f %.6f %.6f %.6f %.6f %.6f %.6f flags %d\n",
			rayProbes[ p ].name, inPt.x, inPt.y, inPt.z, dist,
			outPt.x, outPt.y, outPt.z,
			t.v0().x, t.v0().y, t.v0().z,
			t.v1().x, t.v1().y, t.v1().z,
			t.v2().x, t.v2().y, t.v2().z,
			int( t.flags() ) );
	}

	// ---- NAV: BigWorld.navigatePathPoints over the camera space ----
	static Navigator navigator;
	struct NavProbe
	{
		const char* name;
		Vector3     src;
		Vector3     dst;
	};
	const NavProbe navProbes[] = {
		{ "ACROSS", Vector3( 1.f, 4.f, 1.f ), Vector3( 4.f, 4.f, 1.f ) },
		{ "RAMP",   Vector3( 1.f, 4.f, 1.f ), Vector3( 2.f, 8.f, 8.f ) },
		{ "AROUND", Vector3( 6.f, 4.f, 2.f ), Vector3( 6.f, 4.f, 8.f ) },
		{ "SEAM",   Vector3( 1.f, 4.f, 1.f ), Vector3( 6.f, 4.f, 2.f ) },
		{ "SEAMBA", Vector3( 6.f, 4.f, 2.f ), Vector3( 4.f, 4.f, 1.f ) },
		{ "TOUR",   Vector3( 1.f, 4.f, 1.f ), Vector3( 6.f, 4.f, 8.f ) },
		{ "SAME",   Vector3( 6.f, 4.f, 2.f ), Vector3( 6.f, 4.f, 2.f ) },
	};
	for ( size_t p = 0; p < sizeof( navProbes ) / sizeof( navProbes[ 0 ] );
		++p )
	{
		BW::vector< Vector3 > pathPoints;
		const bool result = navigator.findFullPath( pSpace.get(),
			navProbes[ p ].src, navProbes[ p ].dst, 500.f, true, 0.6f,
			pathPoints );
		if ( !result )
		{
			printf( "NAVMISS %s %.6f %.6f %.6f %.6f %.6f %.6f\n",
				navProbes[ p ].name, navProbes[ p ].src.x,
				navProbes[ p ].src.y, navProbes[ p ].src.z,
				navProbes[ p ].dst.x, navProbes[ p ].dst.y,
				navProbes[ p ].dst.z );
			continue;
		}
		MF_ASSERT( !pathPoints.empty() );
		if ( !almostEqual( pathPoints.front(), navProbes[ p ].src ) )
		{
			pathPoints.insert( pathPoints.begin(), navProbes[ p ].src );
		}
		printf( "NAV %s n %d\n", navProbes[ p ].name,
			int( pathPoints.size() ) );
		for ( size_t i = 0; i < pathPoints.size(); ++i )
		{
			printf( "NAVPT %s %d %.6f %.6f %.6f\n", navProbes[ p ].name,
				int( i ), pathPoints[ i ].x, pathPoints[ i ].y,
				pathPoints[ i ].z );
		}
	}

	printf( "RESULT OK\n" );
	return 0;
}