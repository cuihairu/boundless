// BW-10-05-05 harness: the ORIGINAL generator's saved assets
// (./slope.navmesh, ./shell.navmesh — written by gen_generate.cpp out of
// the unmodified flood+generator+view pipeline) are loaded through the
// ORIGINAL ChunkWaypointSetData::readWaypointSet face exactly as
// navmeshFactory reads .cdata/worldNavmesh, then the fixed-map path
// probes run on the original AStar + NavLoc. The printed truth
// (gen_navpaths.log) is what the Rust port replays.
//
// Compile (gen_waypoints.cpp recipe: reference el7 libs, no generator
// sources — the reader side never touches the generator classes):
//   g++ -std=c++14 -O2 -o gen_navpaths gen_navpaths.cpp \
//     -I $BW/lib -DCODE_INLINE -DCSTDMF_IMPORT -DMF_CONFIG='"hybrid"' \
//     -DMF_SERVER -DMF_USE_ASSERTS -D_HYBRID -DBW_BUILD_PLATFORM='"el7"' \
//     -L $BW/build/el7/lib \
//     -Wl,--start-group -lwaypoint -lchunk -lscene -lresmgr -lmoo \
//     -lmath -lphysics2 -lterrain -lcstdmf -lpng -lz -Wl,--end-group \
//     -lpthread

#include <cmath>
#include <cstdio>
#include <cstdint>
#include <cstring>
#include <vector>

#include "cstdmf/memory_stream.hpp"
#include "resmgr/binary_block.hpp"
#include "resmgr/bwresource.hpp"
#include "resmgr/xml_section.hpp"

#include "math/vector2.hpp"
#include "math/vector3.hpp"
#include "math/vector4.hpp"

#include "waypoint/chunk_waypoint_set_data.hpp"
#include "waypoint/chunk_waypoint_set.hpp"
#include "waypoint/chunk_waypoint_state.hpp"
#include "waypoint/navloc.hpp"
#include "waypoint/chunk_navigator.hpp"
#include "waypoint/astar.hpp"

#include "chunk/chunk.hpp"
#include "chunk/chunk_space.hpp"
#include "chunk/geometry_mapping.hpp"

using namespace std;
using namespace BW;

namespace
{

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

// Walk the record headers the way navmeshFactory's loop does.
int countRecords( const uint8_t* d, size_t len )
{
    int n = 0;
    size_t off = 0;
    while ( off + 16 <= len )
    {
        int32_t nwp = *(const int32_t*)( d + off + 8 );
        int32_t ne  = *(const int32_t*)( d + off + 12 );
        off += 16 + size_t( nwp ) * 12 + size_t( ne ) * 12;
        ++n;
    }
    return n;
}

void dumpSet( const char* name, int record, ChunkWaypointSetData& set )
{
    const ChunkWaypoints& wps = set.waypoints();
    for ( size_t i = 0; i < wps.size(); ++i )
    {
        const ChunkWaypoint& wp = wps[ i ];
        printf( "LWP %s %d %d minh %.6f maxh %.6f edges %d centre %.6f %.6f\n",
            name, record, int( i ), wp.minHeight_, wp.maxHeight_,
            int( wp.edgeCount_ ), wp.centre_.x, wp.centre_.y );
        for ( EdgeIndex e = 0; e < wp.edgeCount_; ++e )
        {
            const ChunkWaypoint::Edge& edge = wp.edges_[ e ];
            const Vector2& v = set.vertexByIndex( edge.vertexIndex_ );
            printf( "LEDGE %s %d %d %d %.6f %.6f %04X\n", name, record,
                int( i ), int( e ), v.x, v.y, edge.neighbour_ );
        }
    }
}

// The navmeshFactory face: per record, a heap-allocated ChunkWaypointSetData
// whose base index accumulates from 1.
std::vector< ChunkWaypointSetData* > loadAsset( const char* name,
    const std::vector< uint8_t >& blob )
{
    printf( "LOAD %s records %d\n", name, countRecords( &blob[ 0 ],
        blob.size() ) );
    std::vector< ChunkWaypointSetData* > sets;
    const uint8_t* ptr = &blob[ 0 ];
    const uint8_t* end = ptr + blob.size();
    int base = 1;
    int record = 0;
    while ( ptr < end )
    {
        ChunkWaypointSetData* pSet = new ChunkWaypointSetData( base );
        int32_t version = *(const int32_t*)ptr; ptr += 4;
        float   girth   = *(const float*)ptr;   ptr += 4;
        int32_t numWps  = *(const int32_t*)ptr; ptr += 4;
        int32_t numEdges= *(const int32_t*)ptr; ptr += 4;
        MF_ASSERT( version == 0 );
        printf( "LHDR %s %d version %d girth %.6f waypoints %d edges %d\n",
            name, record, version, girth, numWps, numEdges );
        pSet->girth( girth );
        ptr = (const uint8_t*)pSet->readWaypointSet( (const char*)ptr,
            numWps, numEdges );
        dumpSet( name, record, *pSet );
        base += int( pSet->waypoints().size() );
        sets.push_back( pSet );
        ++record;
    }
    return sets;
}

struct PathProbe
{
    const char* name;
    Vector3     src;
    Vector3     dst;
};

// The chunk assembly of gen_waypoints.cpp: one chunk mounting every
// loaded set; NavLoc's ctor resolves through the navigator's girth
// filter, so a point over the shell floor lands in record 0's set and a
// point over the bridge lands in record 1's.
void runProbes( const char* name,
    const std::vector< ChunkWaypointSetData* >& sets,
    const std::vector< PathProbe >& probes )
{
    ChunkSpacePtr pSpace = new ChunkSpace( 1 );
    XMLSectionPtr pSettings = new XMLSection( "space" );
    GeometryMapping* pMapping =
        new GeometryMapping( pSpace, Matrix::identity, ".", pSettings );
    BoundingBox localBB( Vector3( -10.f, -10.f, -10.f ),
        Vector3( 50.f, 50.f, 50.f ) );
    Chunk* pChunk =
        new Chunk( name, pMapping, Matrix::identity, localBB );

    for ( size_t i = 0; i < sets.size(); ++i )
    {
        ChunkWaypointSetDataPtr pSetData( sets[ i ] );
        ChunkWaypointSetPtr pWaySet = new ChunkWaypointSet( pSetData );
        pWaySet->chunk( pChunk );
        ChunkNavigator::instance( *pChunk ).add( pWaySet.get() );
        // The reference free list destructs each recycled search state
        // twice; pin the set like gen_waypoints.cpp does.
        for ( int pin = 0; pin < 128; ++pin )
        {
            pWaySet->incRef();
        }
    }

    // Layer probes: containment and find per set at the fixed points.
    // slope owns one set; the shell's two same-girth sets must pick
    // their own layer by height band alone.
    Vector3 layerPts[] = {
        Vector3( 1.f,   0.5f, 1.f   ),
        Vector3( 7.f,   3.f,  7.f   ),
        Vector3( 5.f,   1.f,  5.f   ),
        Vector3( 5.f,   3.f,  5.f   ),
        Vector3( 2.5f,  0.5f, 4.5f  ),
        Vector3( 3.f,   1.5f, 3.5f  ),
    };
    for ( size_t s = 0; s < sets.size(); ++s )
    {
        ChunkWaypointSetData& set = *sets[ s ];
        const ChunkWaypoints& wps = set.waypoints();
        for ( size_t p = 0; p < sizeof( layerPts ) / sizeof( layerPts[ 0 ] );
            ++p )
        {
            printf( "CONTAINS %s %d %d %.6f %.6f %.6f %d\n", name, int( s ),
                int( p ), layerPts[ p ].x, layerPts[ p ].y, layerPts[ p ].z,
                wps.empty() ? 0
                    : int( wps[ 0 ].contains( set,
                        GeoSpaceVector3( layerPts[ p ].x, layerPts[ p ].y,
                            layerPts[ p ].z ) ) ) );
            printf( "FIND %s %d %d %.6f %.6f %.6f %d\n", name, int( s ),
                int( p ), layerPts[ p ].x, layerPts[ p ].y, layerPts[ p ].z,
                set.find( GeoSpaceVector3( layerPts[ p ].x, layerPts[ p ].y,
                    layerPts[ p ].z ) ) );
        }
    }

    for ( size_t p = 0; p < probes.size(); ++p )
    {
        NavLoc src( pChunk, probes[ p ].src, 0.8f );
        NavLoc dst( pChunk, probes[ p ].dst, 0.8f );
        printf( "PATHBEGIN %s %s %d %d\n", name, probes[ p ].name,
            int( src.waypoint() ), int( dst.waypoint() ) );

        ChunkWaypointState srcState( src );
        ChunkWaypointState dstState( dst );
        AStar< ChunkWaypointState,
              ChunkWaypointState::SearchData >::StateScopedFreeList
            freeList;
        AStar< ChunkWaypointState, ChunkWaypointState::SearchData >
            astar( &freeList );
        if ( astar.search( srcState, dstState, -1.f ) )
        {
            for ( const ChunkWaypointState* s = astar.first();
                s != NULL; s = astar.next() )
            {
                printf( "PATH %s %s %.6f %.6f %.6f %d\n", name,
                    probes[ p ].name, s->navLoc().point().x,
                    s->navLoc().point().y, s->navLoc().point().z,
                    int( s->navLoc().waypoint() ) );
            }
        }
        else
        {
            printf( "PATHNONE %s %s\n", name, probes[ p ].name );
        }
        printf( "PATHEND %s %s\n", name, probes[ p ].name );
    }
}

} // anonymous namespace

int main()
{
    BWResource bwResource;
    BWResource::instance().init( BW::vector< BW::string >() );

    {
        std::vector< uint8_t > slope = readFile( "./slope.navmesh" );
        std::vector< ChunkWaypointSetData* > sets = loadAsset( "slope", slope );
        std::vector< PathProbe > probes;
        PathProbe raw[] = {
            // across the shared x=3 edge (P1 -> P0) and back
            { "S1", Vector3( 1.f,  0.5f, 1.f ), Vector3( 7.f,  3.f,  7.f ) },
            { "S2", Vector3( 7.f,  3.f,  7.f ), Vector3( 1.f,  0.5f, 1.f ) },
            // within one polygon
            { "S3", Vector3( 1.f,  0.5f, 1.f ), Vector3( 2.f,  0.5f, 2.f ) },
            { "S4", Vector3( 6.f,  3.f,  6.f ), Vector3( 7.f,  3.f,  7.f ) },
        };
        probes.insert( probes.end(), raw, raw + 4 );
        runProbes( "slope", sets, probes );
    }

    {
        std::vector< uint8_t > shell = readFile( "./shell.navmesh" );
        std::vector< ChunkWaypointSetData* > sets = loadAsset( "shell", shell );
        std::vector< PathProbe > probes;
        PathProbe raw[] = {
            // within the room floor (record 0's set)
            { "F1", Vector3( 2.5f, 0.5f, 4.5f ), Vector3( 3.5f, 0.5f, 2.5f ) },
            // within the bridge (record 1's set)
            { "B1", Vector3( 3.f,  1.5f, 3.5f ), Vector3( 2.5f, 1.5f, 3.f ) },
            // floor -> bridge: the generator gave the layers no
            // inter-set adjacency, so the search must fail
            { "X1", Vector3( 2.5f, 0.5f, 4.5f ), Vector3( 3.f,  1.5f, 3.5f ) },
        };
        probes.insert( probes.end(), raw, raw + 3 );
        runProbes( "shell", sets, probes );
    }

    printf( "RESULT OK\n" );
    return 0;
}
