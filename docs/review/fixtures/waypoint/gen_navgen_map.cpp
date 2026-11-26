// BW-10-07-02 harness: the fixed map (×10 of the 07-01 hand map: ground
// 4.0, hole strip x∈[2,3], ramp steps z∈[5,10] rising 4.5→9.5, obstacle
// slab x∈[6.5,8.5] z∈[4,6] h13) runs through the ORIGINAL navgen
// per-chunk pipeline — chunk_waypoint_generator.cpp:139-194: flood
// (ChunkFlooder::reset face: bb expanded by 2*reso, reso 0.5,
// waypoint_flood.cpp setArea's HACK_OFFSET 0.1 on top), generator init +
// grid copy, generate(), determineEdgesAdjacentToOtherChunks(),
// streamline(), extendThroughUnboundPortals() (no-op: the stub bounds()
// is empty, i.e. both chunks bound like navgen's canProcess 3x3
// neighbourhood), calculateSetMembership(), asBinary. The printed truth
// (gen_navgen_map.log) is what the Rust port's waypoint-gen artifacts
// test replays; the written ./mapA.navmesh / ./mapB.navmesh are what
// gen_navgen_paths.cpp (the original reader, el7 libs) consumes.
//
// build (stubs at /tmp/genstub = shadow chunk/chunk_space.hpp +
// chunk/chunk.hpp + empty pch.hpp + the three generator sources):
//   g++ -std=c++14 -O2 -o gen_navgen_map gen_navgen_map.cpp \
//     /tmp/genstub/waypoint_generator/waypoint_flood.cpp \
//     /tmp/genstub/waypoint_generator/waypoint_generator.cpp \
//     /tmp/genstub/waypoint_generator/waypoint_view.cpp \
//     -I /tmp/genstub -I /tmp/genstub/waypoint_generator -I $BW/lib \
//     -DCODE_INLINE -D_RELEASE -DHANDLE=void* -DINVALID_HANDLE_VALUE=NULL \
//     -DBW_GUARD= $BW/build/el7/lib/libcstdmf.a $BW/build/el7/lib/libmath.a \
//     -lpthread

#include <cmath>
#include <cstdio>
#include <cstring>
#include <vector>

#include "pch.hpp"

#include "waypoint_generator/waypoint_flood.hpp"
#include "waypoint_generator/waypoint_generator.hpp"
#include "waypoint_generator/waypoint_view.hpp"

#include "chunk/chunk.hpp"

using namespace BW;

namespace
{

class Slab
{
public:
    float x0, z0, x1, z1, h;
    Slab( float a, float b, float c, float d, float e )
        : x0( a ), z0( b ), x1( c ), z1( d ), h( e ) {}
};

// identical face to gen_generate.cpp:47-102
class SlabPhysics : public IPhysics
{
public:
    std::vector< Slab > slabs;
    float maxStep;

    explicit SlabPhysics( float step ) : maxStep( step ) {}

    bool topAt( float x, float z, float below, float& out ) const
    {
        bool found = false;
        float best = 0.f;
        for ( size_t i = 0; i < slabs.size(); ++i )
        {
            const Slab& s = slabs[i];
            if ( x >= s.x0 && x < s.x1 && z >= s.z0 && z < s.z1 && s.h <= below )
            {
                if ( !found || s.h > best )
                {
                    best = s.h;
                    found = true;
                }
            }
        }
        if ( !found ) return false;
        out = best;
        return true;
    }

    virtual Vector3 getGirth() const
    {
        return Vector3( 0.5f, 1.5f, 0.5f );
    }

    virtual float getScrambleHeight() const { return 0.5f; }

    virtual bool findDropPoint( const Vector3& pos, float& y )
    {
        return topAt( pos.x, pos.z, pos.y, y );
    }

    virtual void adjustMove(
        const Vector3& src, const Vector3& dst, Vector3& dst2 )
    {
        float h;
        if ( topAt( dst.x, dst.z, dst.y + getScrambleHeight(), h ) &&
            fabs( h - src.y ) <= maxStep )
        {
            dst2 = Vector3( dst.x, h, dst.z );
        }
        else
        {
            dst2 = Vector3( dst.x, -10000.0f, dst.z );
        }
    }
};

// the ×10 fixed map: ground 4.0 with the hole strip x∈[2,3) z∈[0,5),
// ramp steps z∈[5,10) rising 0.5 per half-metre row, obstacle slab
// x∈[6.5,8.5) z∈[4,6) h13 (9 above ground: unreachable, so the flood
// never enters it — the obstacle is an absence the probes pin).
void fillWorld( SlabPhysics& p )
{
    // chunk A south: ground either side of the hole
    p.slabs.push_back( Slab( 0.f, 0.f, 2.f, 5.f, 4.0f ) );
    p.slabs.push_back( Slab( 3.f, 0.f, 5.f, 5.f, 4.0f ) );
    // chunk B ground: south strip, mid ring either side of the obstacle,
    // north strip
    p.slabs.push_back( Slab( 5.f, 0.f, 10.f, 4.f, 4.0f ) );
    p.slabs.push_back( Slab( 5.f, 4.f, 6.5f, 6.f, 4.0f ) );
    p.slabs.push_back( Slab( 8.5f, 4.f, 10.f, 6.f, 4.0f ) );
    p.slabs.push_back( Slab( 5.f, 6.f, 10.f, 10.f, 4.0f ) );
    // chunk A ramp: ten half-metre steps z∈[5,10), x∈[0,5), 4.5→9.5
    for ( int k = 0; k < 10; ++k )
    {
        p.slabs.push_back( Slab( 0.f, 5.f + 0.5f * k, 5.f,
            5.5f + 0.5f * k, 4.5f + 0.5f * k ) );
    }
    // chunk B obstacle: 9 above ground — the flood can never climb it
    p.slabs.push_back( Slab( 6.5f, 4.f, 8.5f, 6.f, 13.0f ) );
}

unsigned long long fnv1a64( const char* data, size_t len )
{
    unsigned long long hash = 14695981039346656037ULL;
    for ( size_t i = 0; i < len; ++i )
    {
        hash ^= (unsigned char)data[i];
        hash *= 1099511628211ULL;
    }
    return hash;
}

// same dumpBinary as gen_generate.cpp:143-186
void dumpBinary( const char* what, BinaryPtr bin )
{
    printf( "%s bytes %zu fnv %016llx\n", what, bin->len(),
        fnv1a64( bin->cdata(), bin->len() ) );
    for ( size_t i = 0; i < bin->len(); i += 16 )
    {
        printf( "HX %04zx ", i );
        char chunk[33];
        for ( size_t j = 0; j < 16 && i + j < bin->len(); ++j )
            sprintf( chunk + j * 2, "%02x", (unsigned char)bin->cdata()[ i + j ] );
        chunk[ 32 ] = 0;
        printf( "%s\n", chunk );
    }
    const char* ptr = bin->cdata();
    const char* end = ptr + bin->len();
    int record = 0;
    while (ptr < end)
    {
        int version = *((int*&)ptr)++;
        float girth = *((float*&)ptr)++;
        int navPolyElts = *((int*&)ptr)++;
        int navPolyEdges = *((int*&)ptr)++;
        printf( "R %d version %d girth %.1f nwp %d ne %d\n",
            record, version, girth, navPolyElts, navPolyEdges );
        const char* ctrl = ptr;
        ptr += navPolyElts * 12;
        for ( int p = 0; p < navPolyElts; ++p )
        {
            const char* c = ctrl + p * 12;
            float minh = *((float*&)c)++;
            float maxh = *((float*&)c)++;
            int vc = *((int*&)c)++;
            printf( "  P %d minh %.1f maxh %.1f vc %d\n", p, minh, maxh, vc );
        }
        for ( int v = 0; v < navPolyEdges; ++v )
        {
            float x = *((float*&)ptr)++;
            float z = *((float*&)ptr)++;
            int adj = *((int*&)ptr)++;
            printf( "  V %d x %.1f z %.1f adj %d\n", v, x, z, adj );
        }
        ++record;
    }
}

// same dumpPolys as gen_generate.cpp:190-208 (the chunk column carries
// VertexRef's adjToAnotherChunk)
void dumpPolys( const char* what, IWaypointView& view )
{
    printf( "%s polys %d sets %d\n", what, view.getPolygonCount(),
        view.getSets() );
    for ( int p = 0; p < view.getPolygonCount(); ++p )
    {
        IWaypointView::PolygonRef poly( view, p );
        printf( "  P %d set %d minh %.1f maxh %.1f vc %u\n", p,
            poly.navPolySetIndex(), poly.minHeight(), poly.maxHeight(),
            poly.size() );
        for ( unsigned v = 0; v < poly.size(); ++v )
        {
            IWaypointView::VertexRef vr = poly[ int( v ) ];
            printf( "  V %u %u x %.2f z %.2f adj %d chunk %d\n", v,
                unsigned( 0 ), vr.pos().x, vr.pos().y, vr.adjNavPoly(),
                int( vr.adjToAnotherChunk() ) );
        }
    }
}

// exposes the protected boundary set the same way the 05-02 fixture does
class DumpFlood : public WaypointFlood
{
public:
    void dumpBoundary()
    {
        for ( BW::set< std::pair< int, int > >::const_iterator it =
                boundarySet_.begin();
            it != boundarySet_.end(); ++it )
        {
            printf( "B %d %d\n", it->first, it->second );
        }
    }
};

// the flood grid dump from gen_flood.cpp:146-181: SCENE line first, then
// per-layer H lines (linear cell index) and bit-decomposed A lines, then
// the boundary set
void dumpGrid( const char* name, DumpFlood& wf, int total )
{
    printf( "SCENE %s count %d\n", name, total );
    const int cells = wf.xsize() * wf.zsize();
    for ( int g = 0; g < int( WaypointFlood::MAX_HEIGHTS ); ++g )
    {
        for ( int i = 0; i < cells; ++i )
        {
            float h = wf.hgtGrids()[ g ][ i ];
            if ( h > -9999.0f )
            {
                printf( "H %d %d %.1f\n", g, i, h );
            }
        }
    }
    for ( int g = 0; g < int( WaypointFlood::MAX_HEIGHTS ); ++g )
    {
        for ( int i = 0; i < cells; ++i )
        {
            uint32_t all = wf.adjGrids()[ g ][ i ].all;
            for ( int a = 0; a < 8; ++a )
            {
                uint32_t v = ( all >> ( a << 2 ) ) & 0xFu;
                if ( v ) printf( "A %d %d %d %u\n", g, i, a, v );
            }
        }
    }
    wf.dumpBoundary();
}

void runChunk( const char* name, float ownsMinX, float ownsMaxX,
    const Vector3& bbMin, const Vector3& bbMax, SlabPhysics& phys,
    const Vector3& seed )
{
    // --- flood stage: ChunkFlooder::reset's face (chunk_flooder.cpp:98-110)
    DumpFlood wf;
    wf.setArea( bbMin - Vector3( 1.f, 0.f, 1.f ),
        bbMax + Vector3( 1.f, 0.f, 1.f ), 0.5f );
    wf.setPhysics( &phys );

    Chunk chunk;
    chunk.ownsMinX_ = ownsMinX;
    chunk.ownsMaxX_ = ownsMaxX;
    chunk.ownsMinZ_ = 0.f;
    chunk.ownsMaxZ_ = 10.f;
    chunk.space_.minX_ = -100.f;
    chunk.space_.maxX_ = 100.f;
    chunk.space_.minZ_ = -100.f;
    chunk.space_.maxZ_ = 100.f;
    wf.setChunk( &chunk );

    int count = wf.fill( seed );
    printf( "FILL 0 count %d\n", count );
    wf.postfilteradd();
    wf.postfilterremove();
    dumpGrid( name, wf, count );
    printf( "SEEDAREA %s min %.1f %.1f size %d %d\n", name, wf.min().x,
        wf.min().z, wf.xsize(), wf.zsize() );

    // --- generator stage: chunk_waypoint_generator.cpp:154-194
    WaypointGenerator gener;
    gener.init( wf.xsize(), wf.zsize(), wf.min(), 0.5f );
    const int cells = wf.xsize() * wf.zsize();
    for ( int g = 0; g < int( WaypointFlood::MAX_HEIGHTS ); ++g )
    {
        memcpy( gener.hgtGrids()[ g ], wf.hgtGrids()[ g ],
            cells * sizeof( float ) );
        memcpy( gener.adjGrids()[ g ], wf.adjGrids()[ g ],
            cells * sizeof( AdjGridElt ) );
    }

    gener.generate();
    dumpPolys( "GENERATED", gener );
    gener.determineEdgesAdjacentToOtherChunks( &chunk, &phys );
    dumpPolys( "DETERMINED", gener );
    gener.streamline();
    dumpPolys( "STREAMLINED", gener );
    gener.extendThroughUnboundPortals( &chunk );
    BW::vector< Vector3 > frontier( 1, seed );
    gener.calculateSetMembership( frontier, BW::string( name ) );
    dumpPolys( "SETMEM", gener );

    BinaryPtr finalStream = gener.asBinary( Matrix::identity, 0.6f );
    dumpBinary( name, finalStream );
    char path[ 64 ];
    snprintf( path, sizeof( path ), "./%s.navmesh", name );
    FILE* nav = fopen( path, "wb" );
    fwrite( finalStream->cdata(), 1, finalStream->len(), nav );
    fclose( nav );
    printf( "WROTE %s %zu bytes\n", name, size_t( finalStream->len() ) );
}

} // anonymous namespace

int main()
{
    SlabPhysics phys( 0.6f );
    fillWorld( phys );

    // chunk A: bb x∈[0,5], seed on the west ground (entity + 1y face,
    // chunk_waypoint_generator.cpp:51-52)
    runChunk( "mapA", 0.f, 5.f, Vector3( 0.f, 0.f, 0.f ),
        Vector3( 5.f, 0.f, 10.f ), phys, Vector3( 1.f, 6.f, 1.f ) );

    // chunk B: bb x∈[5,10], seed on the south strip
    runChunk( "mapB", 5.f, 10.f, Vector3( 5.f, 0.f, 0.f ),
        Vector3( 10.f, 0.f, 10.f ), phys, Vector3( 6.f, 6.f, 2.f ) );

    printf( "RESULT OK\n" );
    return 0;
}
