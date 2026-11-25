// BW-10-07-03 server-load harness: the ORIGINAL server face over the
// ×10 fixed map the navgen pipeline generated in 07-02. Both chunk
// assets load through ChunkWaypointSetData::readWaypointSet exactly as
// navmeshFactory reads them (gen_navgen_paths.cpp's loader face), each
// set mounts on its OWN chunk's navigator, and the chunk-level bind()
// walk connects the seam edges through the permissive portal pair —
// the wiring 07-02's unwired FFFF edges lacked (SEAM is expected to
// path here).
//
// chunk box note: the fixture chunks are inside chunks (5x10), which
// gridFromChunkName cannot express, so they take the loader's FNF
// branch (load(NULL): loaded() true, boxes reset to [0,1]^3) and the
// real world boxes then come in through the public localBB/boundingBox
// setters (chunk.hpp:221-222) — the same hooks the real loader's
// size-setting pass uses. Chunk transforms are identity, so chunk, geo
// and world space share coordinates (MappedVector3 is the identity
// mapping).
//
// portal rect note: the rect spans the flood overlap band (x in
// [4.4, 5.6]) rather than a thin strip on the physical seam plane:
// navgen's seam edges live in the ±1m band (vertices at x = 4.6 / 5.1
// per the 07-02 DETERMINED dump), and bind()'s portal test measures
// the RAW edge midpoint against the rect in the portal's (uAxis,
// vAxis) axes (Chunk::findBetterPortal), so the passage geometry must
// cover the band for the connection to be found. The far-bb intersect
// gate and the projection reconstruction (origin + u*(u.lwv) +
// v*(v.lwv)) stay the real server's.
//
// compile (gen_navgen_paths.cpp recipe: reference el7 libs, no
// generator sources — the server side never touches the generator
// classes):
//   g++ -std=c++14 -O2 -o gen_mapload gen_mapload.cpp \
//     -I $BW/lib -DCODE_INLINE -DCSTDMF_IMPORT -DMF_CONFIG='"hybrid"' \
//     -DMF_SERVER -DMF_USE_ASSERTS -D_HYBRID -DBW_BUILD_PLATFORM='"el7"' \
//     -L $BW/build/el7/lib \
//     -Wl,--start-group -lwaypoint -lchunk -lscene -lresmgr -lmoo \
//     -lmath -lphysics2 -lterrain -lcstdmf -lpng -lz -Wl,--end-group \
//     -lpthread
// run with cwd holding ./mapA.navmesh and ./mapB.navmesh.
//
// Truth lines:
//   LOAD / LHDR / LWP / LEDGE   the decode (terrain truth: heights,
//                               vertices, raw neighbour codes)
//   CHUNKS                      the two chunk boxes and loaded flags
//   PORTALS / PORTAL            the mounted portal pair
//   CONNTO / EDGELABEL          the original bind()'s wiring
//   ITER                        the (born-dead) neighbour walks
//   NAVLOC / NAVLOCNONE         per-chunk NavLoc resolution
//   XPATHBEGIN/XPATH/XPATHNONE/XPATHEND      chunk-level A*
//   SETSEARCHBEGIN/SETHOP/SETSEARCHNONE/SETSEARCHEND  set-level A*
//   RESULT OK

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
#include "waypoint/chunk_waypoint_set_state.hpp"
#include "waypoint/waypoint_neighbour_iterator.hpp"
#include "waypoint/chunk_waypoint_state.hpp"
#include "waypoint/navloc.hpp"
#include "waypoint/chunk_navigator.hpp"
#include "waypoint/astar.hpp"

#include "chunk/chunk.hpp"
#include "chunk/chunk_boundary.hpp"
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

} // anonymous namespace

int main()
{
    BWResource bwResource;
    BWResource::instance().init( BW::vector< BW::string >() );

    std::vector< ChunkWaypointSetData* > setData[2];
    int base = 1;
    {
        std::vector< uint8_t > a = readFile( "./mapA.navmesh" );
        setData[0] = loadAsset( "mapA", a, base );
    }
    {
        std::vector< uint8_t > b = readFile( "./mapB.navmesh" );
        setData[1] = loadAsset( "mapB", b, base );
    }
    MF_ASSERT( setData[0].size() == 1 );
    MF_ASSERT( setData[1].size() == 1 );

    // ---- two inside chunks with their real world boxes ----
    ChunkSpacePtr pSpace = new ChunkSpace( 7 );
    XMLSectionPtr pSettings = new XMLSection( "space" );
    GeometryMapping* pMapping =
        new GeometryMapping( pSpace, Matrix::identity, ".", pSettings );

    Chunk* pChunk[2];
    const char* names[2] = { "chunkA", "chunkB" };
    const BoundingBox boxes[2] = {
        BoundingBox( Vector3( 0.f, 0.f, 0.f ),
                     Vector3( 5.f, 15.f, 10.f ) ),
        BoundingBox( Vector3( 5.f, 0.f, 0.f ),
                     Vector3( 10.f, 15.f, 10.f ) )
    };
    for ( int c = 0; c < 2; ++c )
    {
        pChunk[ c ] = new Chunk( names[ c ], pMapping, Matrix::identity,
            boxes[ c ] );
        // A NULL section takes the loader's FNF branch: loaded() true
        // with the bounding box reset to [0,1]^3.
        pChunk[ c ]->load( NULL );
        // ... and the size pass lands the real boxes through the
        // public setters (chunk.hpp:221-222).
        pChunk[ c ]->localBB( boxes[ c ] );
        pChunk[ c ]->boundingBox( boxes[ c ] );
        printf(
            "CHUNK %s min %.6f %.6f %.6f max %.6f %.6f %.6f loaded %d\n",
            names[ c ], boxes[ c ].minBounds().x, boxes[ c ].minBounds().y,
            boxes[ c ].minBounds().z, boxes[ c ].maxBounds().x,
            boxes[ c ].maxBounds().y, boxes[ c ].maxBounds().z,
            pChunk[ c ]->loaded() ? 1 : 0 );
    }

    // ---- mount each set on its own chunk's navigator ----
    ChunkWaypointSetPtr pWay[2];
    for ( int c = 0; c < 2; ++c )
    {
        pWay[ c ] = new ChunkWaypointSet(
            ChunkWaypointSetDataPtr( setData[ c ][ 0 ] ) );
        pWay[ c ]->chunk( pChunk[ c ] );
        ChunkNavigator::instance( *pChunk[ c ] ).add( pWay[ c ].get() );
        // the reference free list destructs each recycled search state
        // twice; pin the set like gen_navgen_paths.cpp does
        for ( int pin = 0; pin < 512; ++pin )
        {
            pWay[ c ]->incRef();
        }
    }

    // ---- the portal pair on the seam ----
    // PERMISSIVE in both directions; the rect corners are the (u, v)
    // rectangle in clockwise order (findBetterPortal's cross products
    // must all come out positive), covering the flood overlap band in
    // u (see the portal rect note in the header).
    PlaneEq planeAB( Vector3( 1.f, 0.f, 0.f ), 5.f );
    BW::vector< Vector3 > rectAB;
    rectAB.push_back( Vector3( 4.4f, 9.9f, 0.f ) );
    rectAB.push_back( Vector3( 4.4f, 0.1f, 0.f ) );
    rectAB.push_back( Vector3( 5.6f, 0.1f, 0.f ) );
    rectAB.push_back( Vector3( 5.6f, 9.9f, 0.f ) );

    ChunkBoundary::Portal* portals[2];
    // 0: chunk A east -> B (permissive)
    portals[0] = new ChunkBoundary::Portal( planeAB, Vector3( 0, 1, 0 ),
        rectAB, pMapping, "" );
    portals[0]->pChunk = pChunk[1];
    // 1: chunk B west -> A (permissive)
    portals[1] = new ChunkBoundary::Portal( planeAB, Vector3( 0, 1, 0 ),
        rectAB, pMapping, "" );
    portals[1]->pChunk = pChunk[0];

    // bind()'s projection reconstructs the far point as
    // origin + uAxis*(uAxis . lwv) + vAxis*(vAxis . lwv); the identity
    // chunks want x and z preserved and the height landed mid-band
    // (the ground polys are the band [4, 4], so 4.0 + the 0.1 avg
    // bump).
    for ( int i = 0; i < 2; ++i )
    {
        portals[ i ]->uAxis = Vector3( 1.f, 0.f, 0.f );
        portals[ i ]->vAxis = Vector3( 0.f, 0.f, 1.f );
        portals[ i ]->origin = Vector3( 0.f, 4.1f, 0.f );
    }

    // Each boundary mounts its portal straight into the bound list via
    // the engine's own mover, so readyToBind's unbound-portal gate (a
    // plain portal there blocks binding) stays clear.
    ChunkBoundary* pBoundA = new ChunkBoundary( planeAB, portals[0] );
    pBoundA->bindPortal( 0 );
    pChunk[0]->joints().push_back( ChunkBoundaryPtr( pBoundA ) );

    ChunkBoundary* pBoundB = new ChunkBoundary( planeAB, portals[1] );
    pBoundB->bindPortal( 0 );
    pChunk[1]->joints().push_back( ChunkBoundaryPtr( pBoundB ) );

    printf( "PORTALS 2\n" );
    for ( int i = 0; i < 2; ++i )
    {
        printf( "PORTAL %d permissive %d haschunkitem %d far %d\n", i,
            portals[ i ]->permissive ? 1 : 0,
            portals[ i ]->hasChunkItem() ? 1 : 0,
            portals[ i ]->pChunk == pChunk[ (i + 1) % 2 ] ? (i + 1) % 2 : -1 );
    }

    // ---- the original bind: real connections on both sides ----
    for ( int c = 0; c < 2; ++c )
    {
        pWay[ c ]->bind();
    }

    // set identity helper: pointer -> index
    ChunkWaypointSet* wayPtr[2] = { pWay[0].get(), pWay[1].get() };
    auto setIdxOf = [&]( ChunkWaypointSet* p ) -> int {
        for ( int c = 0; c < 2; ++c )
            if ( p == wayPtr[ c ] ) return c;
        return -1;
    };
    auto portalIdxOf = [&]( ChunkBoundary::Portal* p ) -> int {
        for ( int i = 0; i < 2; ++i )
            if ( p == portals[ i ] ) return i;
        return -1;
    };

    // ---- the created connections and edge labels, straight off the
    // reference containers ----
    for ( int c = 0; c < 2; ++c )
    {
        int n = 0;
        for ( ChunkWaypointConns::const_iterator it =
                pWay[ c ]->connectionsBegin();
              it != pWay[ c ]->connectionsEnd(); ++it )
        {
            printf( "CONNTO %d %d %d %d\n", c, n++,
                setIdxOf( it->first.get() ), portalIdxOf( it->second ) );
        }
        for ( ChunkWaypointEdgeLabels::const_iterator it =
                pWay[ c ]->edgeLabelsBegin();
              it != pWay[ c ]->edgeLabelsEnd(); ++it )
        {
            const EdgeLabel& l = it->second;
            printf( "EDGELABEL %d %d %d %d %.6f %.6f %.6f\n", c,
                int( it->first ), setIdxOf( l.pSet_.get() ),
                int( l.waypoint_ ), l.entryPoint_.x, l.entryPoint_.y,
                l.entryPoint_.z );
        }
    }

    // ---- WaypointNeighbourIterator walks, straight off the reference
    // iterator ----
    for ( int c = 0; c < 2; ++c )
    {
        ChunkWaypointSetData& setDataC = *setData[ c ][ 0 ];
        for ( size_t i = 0; i < setDataC.waypoints().size(); ++i )
        {
            printf( "ITER %d %d", c, int( i ) );
            int n = 0;
            for ( WaypointNeighbourIterator wni( pWay[ c ],
                     WaypointIndex( i ) );
                  !wni.ended(); wni.advance() )
            {
                printf( " %d:%d", setIdxOf( wni.pNeighbourSet().get() ),
                    int( wni.neighbourWaypointIndex() ) );
                ++n;
            }
            printf( " (count %d)\n", n );
        }
    }

    // ---- where the original NavLoc resolver drops each probe point,
    // asked of the chunk the point belongs to ----
    {
        struct LocProbe
        {
            const char* name;
            int         chunk;
            Vector3     point;
        };
        const LocProbe probes[] = {
            { "AWEST",  0, Vector3( 1.f,  4.f, 1.f ) },
            { "AEAST",  0, Vector3( 4.f,  4.f, 1.f ) },
            { "ATOP",   0, Vector3( 2.f,  8.f, 8.f ) },
            // the hole centre: no poly contains it, the closest
            // fallback answers
            { "AHOLE",  0, Vector3( 2.5f, 4.f, 1.f ) },
            { "BSOUTH", 1, Vector3( 6.f,  4.f, 2.f ) },
            { "BNORTH", 1, Vector3( 6.f,  4.f, 8.f ) },
            { "BFLANK", 1, Vector3( 5.5f, 4.f, 5.f ) },
            // the obstacle centre: same fallback face on chunk B
            { "BOBST",  1, Vector3( 7.5f, 4.f, 5.f ) },
            // dead on the seam plane, asked of chunk B
            { "BSEAM",  1, Vector3( 5.f,  4.f, 2.f ) },
        };
        for ( size_t p = 0; p < sizeof( probes ) / sizeof( probes[0] );
            ++p )
        {
            NavLoc loc( pChunk[ probes[ p ].chunk ], probes[ p ].point,
                0.6f );
            if ( loc.pSet() )
            {
                printf( "NAVLOC %s %d %.6f %.6f %.6f wp %d set %d y %.6f\n",
                    probes[ p ].name, probes[ p ].chunk, probes[ p ].point.x,
                    probes[ p ].point.y, probes[ p ].point.z,
                    int( loc.waypoint() ),
                    setIdxOf( loc.pSet().get() ), loc.point().y );
            }
            else
            {
                printf( "NAVLOCNONE %s %d\n", probes[ p ].name,
                    probes[ p ].chunk );
            }
        }
    }

    // ---- chunk-level A* probes ----
    {
        struct PathProbe
        {
            const char* name;
            int         srcChunk;
            Vector3     src;
            int         dstChunk;
            Vector3     dst;
        };
        const PathProbe probes[] = {
            // across chunk A west->east: the hole forces the ramp
            // detour (07-02's ACROSS, unchanged)
            { "ACROSS", 0, Vector3( 1.f, 4.f, 1.f ),
                        0, Vector3( 4.f, 4.f, 1.f ) },
            // west ground up to the ramp top band
            { "RAMP",   0, Vector3( 1.f, 4.f, 1.f ),
                        0, Vector3( 2.f, 8.f, 8.f ) },
            // across chunk B south->north around the obstacle
            { "AROUND", 1, Vector3( 6.f, 4.f, 2.f ),
                        1, Vector3( 6.f, 4.f, 8.f ) },
            // over the seam through the portal wiring (07-02 failed
            // this with the unwired FFFF edges)
            { "SEAM",   0, Vector3( 1.f, 4.f, 1.f ),
                        1, Vector3( 6.f, 4.f, 2.f ) },
            // ... and back: getAdjacency refuses to expand a state that
            // just crossed a portal, so a chunk-level cross-chunk path
            // must END at the crossing — this probe's goal (A wp3, the
            // AEAST ground) is exactly where B's wired edge lands
            { "SEAMBA", 1, Vector3( 6.f, 4.f, 2.f ),
                        0, Vector3( 4.f, 4.f, 1.f ) },
            // the long tour wants to KEEP GOING after the crossing,
            // which the one-crossing rule refuses (the crossing lands
            // on B wp5, the goal wp8 needs more hops) — XPATHNONE is
            // the reference's answer, not a wiring gap
            { "TOUR",   0, Vector3( 1.f, 4.f, 1.f ),
                        1, Vector3( 6.f, 4.f, 8.f ) },
        };
        for ( size_t p = 0; p < sizeof( probes ) / sizeof( probes[0] );
            ++p )
        {
            NavLoc src( pChunk[ probes[ p ].srcChunk ], probes[ p ].src,
                0.6f );
            NavLoc dst( pChunk[ probes[ p ].dstChunk ], probes[ p ].dst,
                0.6f );
            printf( "XPATHBEGIN %s %d %d\n", probes[ p ].name,
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
                for ( const ChunkWaypointState* sSt = astar.first();
                    sSt != NULL; sSt = astar.next() )
                {
                    printf( "XPATH %s %.6f %.6f %.6f %d %d\n",
                        probes[ p ].name, sSt->navLoc().point().x,
                        sSt->navLoc().point().y, sSt->navLoc().point().z,
                        int( sSt->navLoc().waypoint() ),
                        setIdxOf( sSt->navLoc().pSet().get() ) );
                }
            }
            else
            {
                printf( "XPATHNONE %s\n", probes[ p ].name );
            }
            printf( "XPATHEND %s\n", probes[ p ].name );
        }
    }

    // ---- set-level A* over the connections ----
    {
        struct SetProbe
        {
            const char* name;
            int         srcChunk;
            Vector3     src;
            int         dstChunk;
            Vector3     dst;
            int         blockNonPermissive;
        };
        const SetProbe probes[] = {
            // the permissive seam answers regardless of the block flag
            { "SEAM",   0, Vector3( 1.f, 4.f, 1.f ),
                        1, Vector3( 6.f, 4.f, 2.f ), 0 },
            { "SEAMB",  0, Vector3( 1.f, 4.f, 1.f ),
                        1, Vector3( 6.f, 4.f, 2.f ), 1 },
            { "SEAMBA", 1, Vector3( 6.f, 4.f, 2.f ),
                        0, Vector3( 1.f, 4.f, 1.f ), 0 },
            // same-set sanity walks
            { "WITHINA", 0, Vector3( 1.f, 4.f, 1.f ),
                         0, Vector3( 4.f, 4.f, 1.f ), 0 },
            { "WITHINB", 1, Vector3( 6.f, 4.f, 2.f ),
                         1, Vector3( 6.f, 4.f, 8.f ), 0 },
        };
        for ( size_t p = 0; p < sizeof( probes ) / sizeof( probes[0] );
            ++p )
        {
            NavLoc src( pChunk[ probes[ p ].srcChunk ], probes[ p ].src,
                0.6f );
            NavLoc dst( pChunk[ probes[ p ].dstChunk ], probes[ p ].dst,
                0.6f );
            ChunkWPSetState srcState( src );
            ChunkWPSetState dstState( dst );
            srcState.blockNonPermissive(
                probes[ p ].blockNonPermissive != 0 );
            printf( "SETSEARCHBEGIN %s %d %d %d\n", probes[ p ].name,
                probes[ p ].blockNonPermissive,
                setIdxOf( srcState.pSet().get() ),
                setIdxOf( dstState.pSet().get() ) );

            AStar< ChunkWPSetState >::StateScopedFreeList setFreeList;
            AStar< ChunkWPSetState > astarSet( &setFreeList );
            if ( astarSet.search( srcState, dstState, -1.f ) )
            {
                // first()/next() share one cursor, so the path is
                // collected in a single walk before printing.
                std::vector< const ChunkWPSetState* > chain;
                for ( const ChunkWPSetState* sSt = astarSet.first();
                    sSt != NULL; sSt = astarSet.next() )
                {
                    chain.push_back( sSt );
                }
                for ( size_t seq = 0; seq < chain.size(); ++seq )
                {
                    const ChunkWPSetState* sSt = chain[ seq ];
                    int hop = -1;
                    if ( seq > 0 &&
                        chain[ seq - 1 ]->pSet()->connectionPortal(
                            sSt->pSet() ) != NULL )
                    {
                        hop = portalIdxOf( chain[ seq - 1 ]->pSet()->
                            connectionPortal( sSt->pSet() ) );
                    }
                    printf(
                        "SETHOP %s %d %d %.6f %.6f %.6f %d %d %d\n",
                        probes[ p ].name, int( seq ),
                        setIdxOf( sSt->pSet().get() ),
                        sSt->position().x, sSt->position().y,
                        sSt->position().z,
                        sSt->passedShellBoundary() ? 1 : 0,
                        sSt->passedActivatedPortal() ? 1 : 0, hop );
                }
            }
            else
            {
                printf( "SETSEARCHNONE %s\n", probes[ p ].name );
            }
            printf( "SETSEARCHEND %s\n", probes[ p ].name );
        }
    }

    printf( "RESULT OK\n" );

    return 0;
}
