// BW-10-05-02 fixture generator: the original WaypointFlood runs the two
// acceptance scenes (a 0.5-step slope and an indoor/outdoor shell with
// walls, a west doorway, an indoor bridge that produces second height
// layers, and a chunk ownership edge that records boundary points) and
// prints the grid truth the Rust port's flood::artifacts test asserts on.
//
// Build recipe (prebuilt el7 libs; stubs/ shadow the chunk/cstdmf faces
// the flood never uses beyond Chunk::owns and space min/max coords):
//
//   cp $BW/lib/waypoint_generator/waypoint_flood.{hpp,cpp} /tmp/floodsrc/
//   sed -i 's|\t\tBW::set<std::pair<int, int> > boundarySet_;|\tpublic:\n\t\tBW::set<std::pair<int, int> > boundarySet_;|' \
//     /tmp/floodsrc/waypoint_flood.hpp
//   echo > /tmp/floodsrc/pch.hpp
//   g++ -std=c++14 -O2 -o gen_flood gen_flood.cpp /tmp/floodsrc/waypoint_flood.cpp \
//     -I stubs -I /tmp/floodsrc -I $BW/programming/bigworld/lib \
//     -DHANDLE=void* -DINVALID_HANDLE_VALUE=NULL -DBW_GUARD= \
//     $BW/programming/bigworld/build/el7/lib/libcstdmf.a \
//     $BW/programming/bigworld/build/el7/lib/libmath.a -lpthread
//
// The physics stand-in mirrors the Rust test stub exactly: horizontal
// slabs (half-open x/z ranges), topmost surface at or below the probe,
// adjustMove raises by the scramble height before dropping and marks
// impossible moves with y = -10000 (physics_handler.cpp:221,328,337
// seam, max step 0.6).

#include <cmath>
#include <cstdio>
#include <cstdint>
#include <vector>

// the harness compiles against a copy of waypoint_flood.{hpp,cpp} whose
// boundarySet_ declaration is preceded by a single `public:` line (the
// reference keeps it private; the copy is generated at harness-build
// time and member layout is untouched)
#include "waypoint_generator/waypoint_flood.hpp"

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

// exposes the protected boundary set the same way the Rust port does
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

unsigned long long fnv1a64( const char* path, long* sizeOut )
{
    FILE* f = fopen( path, "rb" );
    unsigned long long h = 14695981039346656037ULL;
    *sizeOut = 0;
    if ( !f ) return h;
    fseek( f, 0, SEEK_END );
    *sizeOut = ftell( f );
    fseek( f, 0, SEEK_SET );
    int c;
    while ( ( c = fgetc( f ) ) != EOF )
    {
        h ^= (unsigned char)c;
        h *= 1099511628211ULL;
    }
    fclose( f );
    return h;
}

void dumpTGA( const char* name, const char* path )
{
    long size = 0;
    unsigned long long h = fnv1a64( path, &size );
    printf(
        "TGA %s size %ld fnv %016llx\n", name, size, h );
}

void dumpGrid( const char* name, DumpFlood& wf, int total )
{
    printf( "SCENE %s count %d\n", name, total );
    const int cells = wf.xsize() * wf.zsize();
    for ( int g = 0; g < int( WaypointFlood::MAX_HEIGHTS ); ++g )
    {
        for ( int i = 0; i < cells; ++i )
        {
            float h = wf.hgtGrids()[g][i];
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
            uint32_t all = wf.adjGrids()[g][i].all;
            for ( int a = 0; a < 8; ++a )
            {
                uint32_t v = ( all >> ( a << 2 ) ) & 0xFu;
                if ( v ) printf( "A %d %d %d %u\n", g, i, a, v );
            }
        }
    }
    wf.dumpBoundary();
}

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

// outdoor ground strips, a sealed room at floor 0.5 (unclimbable 3.0
// walls with no walkable surface inside their footprint) and an indoor
// bridge at 1.5 that stacks second height layers over the floor. The
// room is sealed on purpose: the reference seed-push guard skips cells
// that already carry adjacency (waypoint_flood.cpp:470-472), so the
// bridge/floor layers must come from seeds on virgin cells.
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

void runScene( const char* name, const SlabPhysics& phys, bool clipChunk,
    const Vector3* seeds, int nseeds, const char* tgaPath )
{
    DumpFlood wf;
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
        int c = wf.fill( seeds[i] );
        printf( "FILL %d count %d\n", i, c );
        total += c;
    }

    wf.postfilteradd();
    wf.postfilterremove();

    dumpGrid( name, wf, total );
    wf.writeTGA( tgaPath );
    dumpTGA( name, tgaPath );
}

} // anonymous namespace

int main()
{
    SlabPhysics slope = slopeScene();
    remove( "/tmp/gen_flood_slope.tga" );
    Vector3 slopeSeeds[] = { Vector3( 0.5f, 10.f, 0.5f ) };
    runScene( "slope", slope, false, slopeSeeds, 1, "/tmp/gen_flood_slope.tga" );

    SlabPhysics shell = shellScene();
    remove( "/tmp/gen_flood_shell.tga" );
    // seed quantise floors to the low corner then adds DROP_FUDGE (the
    // setArea HACK_OFFSET puts min_ at 0.1), so seed 2 probes (2.1,3.1):
    // on the bridge (drops onto 1.5), and seed 3 probes (2.1,4.1) from
    // y=1.1: under the bridge onto the room floor (0.5), which stacks a
    // second height layer under the bridge surface
    Vector3 shellSeeds[] = { Vector3( 0.5f, 10.f, 0.5f ),
        Vector3( 3.0f, 10.f, 3.5f ), Vector3( 2.5f, 1.0f, 4.5f ) };
    runScene( "shell", shell, true, shellSeeds, 3, "/tmp/gen_flood_shell.tga" );
    return 0;
}
