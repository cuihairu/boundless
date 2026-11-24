// BW-10-05-04 generator-slice harness: the ORIGINAL WaypointFlood fills
// the two 05-02 scenes (slope + shell), the grids are copied into the
// ORIGINAL WaypointGenerator, and the pipeline init -> generate() ->
// streamline() -> calculateSetMembership() -> asBinary runs unmodified.
// The dump (polygon stream after generate and after streamline, plus the
// record stream with per-16-byte hex lines) is the truth the Rust BSP
// port asserts on.
//
// build (stubs at /tmp/genstub = /tmp/viewstub + floodstub's
// cstdmf/{bw_set,bgtask_manager}.hpp; patched flood copy as in 05-02):
//   g++ -std=c++14 -O2 -o gen_generate gen_generate.cpp \
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

// identical to the 05-02 fixture physics (gen_flood.cpp:52-107)
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

SlabPhysics slopeScene()
{
    SlabPhysics p( 0.6f );
    for ( int i = 0; i < 8; ++i )
    {
        p.slabs.push_back(
            Slab( float( i ), -100.f, float( i + 1 ), 100.f, 0.5f * i ) );
    }
    return p;
}

SlabPhysics shellScene()
{
    SlabPhysics p( 0.6f );
    p.slabs.push_back( Slab( -100.f, -100.f, 0.5f, 100.f, 0.0f ) );
    p.slabs.push_back( Slab( 6.5f, -100.f, 100.f, 100.f, 0.0f ) );
    p.slabs.push_back( Slab( 0.5f, -100.f, 100.f, 0.5f, 0.0f ) );
    p.slabs.push_back( Slab( 0.5f, 6.5f, 100.f, 100.f, 0.0f ) );
    p.slabs.push_back( Slab( 1.5f, 1.5f, 5.5f, 5.5f, 0.5f ) );
    p.slabs.push_back( Slab( 0.5f, 0.5f, 1.5f, 6.5f, 3.0f ) );
    p.slabs.push_back( Slab( 0.5f, 0.5f, 6.5f, 1.5f, 3.0f ) );
    p.slabs.push_back( Slab( 0.5f, 5.5f, 6.5f, 6.5f, 3.0f ) );
    p.slabs.push_back( Slab( 5.5f, 0.5f, 6.5f, 6.5f, 3.0f ) );
    p.slabs.push_back( Slab( 1.5f, 2.5f, 4.5f, 4.5f, 1.5f ) );
    return p;
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

// same dumpBinary as gen_view.cpp: [header][ctrl][data] decode + HX hex
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

// polygon dump through the IWaypointView face (VertexRef applies the
// grid scaling exactly as asBinary sees it)
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

void runScene( const char* name, const SlabPhysics& phys, bool clipChunk,
    const Vector3* seeds, int nseeds )
{
    // --- flood stage (identical to gen_flood.cpp)
    WaypointFlood wf;
    wf.setArea( Vector3( 0.f, 0.f, 0.f ), Vector3( 8.f, 0.f, 8.f ), 1.0f );
    wf.setPhysics( const_cast< SlabPhysics* >( &phys ) );

    Chunk chunk;
    if ( clipChunk )
    {
        chunk.ownsMinX_ = -100.f;
        chunk.ownsMaxX_ = 3.5f;
    }
    else
    {
        chunk.ownsMinX_ = -100.f;
        chunk.ownsMaxX_ = 100.f;
    }
    chunk.ownsMinZ_ = -100.f;
    chunk.ownsMaxZ_ = 100.f;
    chunk.space_.minX_ = -100.f;
    chunk.space_.maxX_ = 100.f;
    chunk.space_.minZ_ = -100.f;
    chunk.space_.maxZ_ = 100.f;
    wf.setChunk( &chunk );

    int total = 0;
    for ( int i = 0; i < nseeds; ++i )
    {
        int c = wf.fill( seeds[ i ] );
        printf( "FILL %d count %d\n", i, c );
        total += c;
    }
    wf.postfilteradd();
    wf.postfilterremove();
    printf( "SCENE %s count %d\n", name, total );

    // --- generator stage: grid copy, generate, streamline, sets
    WaypointGenerator gener;
    gener.init( wf.xsize(), wf.zsize(), Vector3( 0.f, 0.f, 0.f ), 1.0f );
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
    dumpBinary( "GENASBIN", gener.asBinary( Matrix::identity, 0.8f ) );

    gener.streamline();
    dumpPolys( "STREAMLINED", gener );

    BW::vector< Vector3 > frontier( seeds, seeds + nseeds );
    gener.calculateSetMembership( frontier, BW::string( name ) );
    dumpPolys( "SETMEM", gener );
    dumpBinary( name, gener.asBinary( Matrix::identity, 0.8f ) );
}

} // anonymous namespace

int main()
{
    SlabPhysics slope = slopeScene();
    Vector3 slopeSeeds[] = { Vector3( 0.5f, 10.f, 0.5f ) };
    runScene( "slope", slope, false, slopeSeeds, 1 );

    SlabPhysics shell = shellScene();
    Vector3 shellSeeds[] = { Vector3( 0.5f, 10.f, 0.5f ),
        Vector3( 3.0f, 10.f, 3.5f ), Vector3( 2.5f, 1.0f, 4.5f ) };
    runScene( "shell", shell, true, shellSeeds, 3 );
    return 0;
}
