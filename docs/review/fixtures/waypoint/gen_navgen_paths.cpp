// BW-10-07-02 consumer harness: the ORIGINAL reader face
// (ChunkWaypointSetData::readWaypointSet, exactly as navmeshFactory reads
// .cdata/worldNavmesh — gen_navpaths.cpp's structure) reloads the
// ./mapA.navmesh and ./mapB.navmesh streams the generation harness (or,
// for the acceptance leg, the Rust port's byte-identical export) wrote,
// mounts every loaded set on one chunk, and answers containment, NavLoc
// and A* path probes over the ×10 fixed map. The seam edges carry
// 65535 (another chunk) and stay unwired, so the cross-chunk probe must
// answer PATHNONE — wiring them through the portal is 07-03's face.
//
// compile (gen_navpaths.cpp recipe: reference el7 libs, no generator
// sources — the reader side never touches the generator classes):
//   g++ -std=c++14 -O2 -o gen_navgen_paths gen_navgen_paths.cpp \
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

// walk the record headers the way navmeshFactory's loop does
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

// the navmeshFactory face: per record, a heap-allocated
// ChunkWaypointSetData whose base index accumulates across both chunks
std::vector< ChunkWaypointSetData* > loadAsset( const char* name,
    const std::vector< uint8_t >& blob, int& base )
{
    printf( "LOAD %s records %d\n", name, countRecords( &blob[ 0 ],
        blob.size() ) );
    std::vector< ChunkWaypointSetData* > sets;
    const uint8_t* ptr = &blob[ 0 ];
    const uint8_t* end = ptr + blob.size();
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

// the gen_navpaths.cpp assembly: one chunk mounting every loaded set
void runProbes( const char* name,
    const std::vector< ChunkWaypointSetData* >& sets,
    const Vector3 navPts[], int nnavPts,
    const PathProbe probes[], int nprobes )
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
        // the reference free list destructs each recycled search state
        // twice; pin the set like gen_navpaths.cpp does
        for ( int pin = 0; pin < 128; ++pin )
        {
            pWaySet->incRef();
        }
    }

    // containment and find per set at the fixed nav points
    for ( size_t s = 0; s < sets.size(); ++s )
    {
        ChunkWaypointSetData& set = *sets[ s ];
        const ChunkWaypoints& wps = set.waypoints();
        for ( int p = 0; p < nnavPts; ++p )
        {
            printf( "CONTAINS %s %d %d %.6f %.6f %.6f %d\n", name, int( s ),
                int( p ), navPts[ p ].x, navPts[ p ].y, navPts[ p ].z,
                wps.empty() ? 0
                    : int( wps[ 0 ].contains( set,
                        GeoSpaceVector3( navPts[ p ].x, navPts[ p ].y,
                            navPts[ p ].z ) ) ) );
            printf( "FIND %s %d %d %.6f %.6f %.6f %d\n", name, int( s ),
                int( p ), navPts[ p ].x, navPts[ p ].y, navPts[ p ].z,
                set.find( GeoSpaceVector3( navPts[ p ].x, navPts[ p ].y,
                    navPts[ p ].z ) ) );
        }
    }

    // NavLoc resolution: which waypoint each probe point lands in
    for ( int p = 0; p < nnavPts; ++p )
    {
        NavLoc loc( pChunk, navPts[ p ], 0.6f );
        if ( loc.valid() )
        {
            printf( "NAVLOC %s %d %.6f %.6f %.6f wp %d y %.6f\n", name,
                int( p ), navPts[ p ].x, navPts[ p ].y, navPts[ p ].z,
                int( loc.waypoint() ), loc.point().y );
        }
        else
        {
            printf( "NAVLOCNONE %s %d\n", name, int( p ) );
        }
    }

    for ( int p = 0; p < nprobes; ++p )
    {
        NavLoc src( pChunk, probes[ p ].src, 0.6f );
        NavLoc dst( pChunk, probes[ p ].dst, 0.6f );
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

    std::vector< ChunkWaypointSetData* > allSets;
    int base = 1;

    {
        std::vector< uint8_t > a = readFile( "./mapA.navmesh" );
        std::vector< ChunkWaypointSetData* > sets =
            loadAsset( "mapA", a, base );
        allSets.insert( allSets.end(), sets.begin(), sets.end() );
    }
    {
        std::vector< uint8_t > b = readFile( "./mapB.navmesh" );
        std::vector< ChunkWaypointSetData* > sets =
            loadAsset( "mapB", b, base );
        allSets.insert( allSets.end(), sets.begin(), sets.end() );
    }

    // nav points: 0 A west ground, 1 A east ground, 2 A ramp top,
    // 3 A hole centre, 4 B south strip, 5 B north strip,
    // 6 B mid-west flank, 7 B obstacle centre, 8 seam
    Vector3 navPts[] = {
        Vector3( 1.f,   4.f,  1.f   ),
        Vector3( 4.f,   4.f,  1.f   ),
        Vector3( 2.f,   8.f,  8.f   ),
        Vector3( 2.5f,  4.f,  1.f   ),
        Vector3( 6.f,   4.f,  2.f   ),
        Vector3( 6.f,   4.f,  8.f   ),
        Vector3( 5.5f,  4.f,  5.f   ),
        Vector3( 7.5f,  4.f,  5.f   ),
        Vector3( 5.f,   4.f,  2.f   ),
    };

    PathProbe raw[] = {
        // across chunk A west->east: the hole forces the ramp detour
        { "ACROSS", Vector3( 1.f,  4.f,  1.f ), Vector3( 4.f,  4.f,  1.f ) },
        // west ground up to the ramp top band
        { "RAMP",   Vector3( 1.f,  4.f,  1.f ), Vector3( 2.f,  8.f,  8.f ) },
        // across chunk B south->north around the obstacle absence
        { "AROUND", Vector3( 6.f,  4.f,  2.f ), Vector3( 6.f,  4.f,  8.f ) },
        // over the chunk seam: the 65535 edges are unwired, must fail
        { "SEAM",   Vector3( 1.f,  4.f,  1.f ), Vector3( 6.f,  4.f,  2.f ) },
    };
    runProbes( "genmap", allSets, navPts, 9, raw, 4 );

    printf( "RESULT OK\n" );
    return 0;
}
