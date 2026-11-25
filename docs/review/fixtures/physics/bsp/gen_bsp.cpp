// gen_bsp.cpp — BW-10-03-04 fixtures.
//
// 1. built.bsp — the original BSPTreeTool::buildBSP constructs a tree from
//    11 triangles (two tetrahedra, a straddler, a WATER triangle and a
//    NOT_IN_BSP triangle), so partitionNode splits past
//    MAX_TRIANGLES_PER_NODE (10): a z=0 root keeping the two on-plane
//    triangles, a front child with eight and a back child with one (no
//    shared pointers — the shared-pointer face is exercised by the legacy
//    conversion fixtures). BSPTreeTool::saveBSPInMemory writes the
//    current-format blob end to end. Truth lines (BT, f32 as hex bits).
// 2. legacy.bsp — a legacy-format blob crafted per loadBSPLegacy's byte
//    layout, loaded through the same original loadBSP; truth lines (LG)
//    plus the original save of the converted tree (legacy_saved.bsp).
// 3. unit_cube.bsp — the bsp2 child extracted from the real
//    unit_cube.primitives with the original BinSection parser, then loaded
//    through the original loadBSP (12 triangles, 2 nodes, no shared, no
//    user data — a real asset the port must replay; truth lines UC).
//
// Usage: gen_bsp <primitives_path> <out_dir>

#include <cstdio>
#include <cstring>
#include <cstdint>
#include <vector>

#include "resmgr/bin_section.hpp"
#include "resmgr/binary_block.hpp"
#include "resmgr/datasection.hpp"
#include "physics2/bsp.hpp"
#include "physics2/worldtri.hpp"
#include "math/planeeq.hpp"
#include "math/boundbox.hpp"

using namespace std;
using namespace BW;

static void put(std::vector<uint8_t>& b, const void* p, size_t n)
{
    const uint8_t* q = static_cast<const uint8_t*>(p);
    b.insert(b.end(), q, q + n);
}

static void put_u32(std::vector<uint8_t>& b, uint32_t v) { put(b, &v, 4); }
static void put_u16(std::vector<uint8_t>& b, uint16_t v) { put(b, &v, 2); }
static void put_u8(std::vector<uint8_t>& b, uint8_t v) { b.push_back(v); }

static void put_plane(std::vector<uint8_t>& b, const PlaneEq& p)
{
    const Vector3 n = p.normal();
    const float d = p.d();
    put(b, &n, 12);
    put(b, &d, 4);
}

static void put_tri(std::vector<uint8_t>& b, const Vector3& a, const Vector3& c,
    const Vector3& d, WorldTriangle::Flags f)
{
    put(b, &a, 12);
    put(b, &c, 12);
    put(b, &d, 12);
    put_u16(b, static_cast<uint16_t>(f));
    put_u16(b, 0); // padding
}

static void put_user(std::vector<uint8_t>& b, uint32_t key, const char* data)
{
    put_u32(b, key);
    put_u32(b, static_cast<uint32_t>(strlen(data)));
    put(b, data, strlen(data));
}

static uint32_t bits(float f)
{
    uint32_t u;
    memcpy(&u, &f, 4);
    return u;
}

static void dump_tree(const char* tag, BSPTree* tree)
{
    printf("%s NT %u\n", tag, tree->numTriangles());
    printf("%s NN %u\n", tag, tree->numNodes());
    const BoundingBox& bb = tree->boundingBox();
    printf("%s BB %08x %08x %08x %08x %08x %08x\n", tag,
        bits(bb.minBounds().x), bits(bb.minBounds().y), bits(bb.minBounds().z),
        bits(bb.maxBounds().x), bits(bb.maxBounds().y), bits(bb.maxBounds().z));
    printf("%s CAN %d\n", tag, tree->canCollide() ? 1 : 0);
    for (uint i = 0; i < tree->numTriangles(); ++i)
    {
        const WorldTriangle& t = tree->triangles()[i];
        printf("%s TRI %u %08x %08x %08x %08x %08x %08x %08x %08x %08x %04x\n", tag, i,
            bits(t.v0().x), bits(t.v0().y), bits(t.v0().z),
            bits(t.v1().x), bits(t.v1().y), bits(t.v1().z),
            bits(t.v2().x), bits(t.v2().y), bits(t.v2().z),
            t.flags());
    }
    for (uint32_t key = 0; key <= 3; ++key)
    {
        BinaryPtr p = tree->getUserData(key);
        if (p)
        {
            printf("%s USER %u %u", tag, key, p->len());
            for (uint i = 0; i < p->len(); ++i)
            {
                printf(" %02x", static_cast<uint8_t>(p->cdata()[i]));
            }
            printf("\n");
        }
    }
}

// BW-10-03-05: one ray probe through the original BSPTree::intersects —
// the caller seeds dist (the running closest-hit fraction, here the seed
// below) and the walk narrows it to the hit fraction. HIT lines carry the
// seed/dist/hit-point/normal bits and the triangle flags; MISS lines
// carry the seed and the untouched dist.
static void ray_probe(const char* tag, BSPTree* tree,
    const Vector3& s, const Vector3& e, float seed)
{
    const Vector3 delta = e - s;
    float dist = seed;
    const WorldTriangle* pHit = NULL;
    const bool r = tree->intersects(s, e, dist, &pHit);
    if (r && pHit)
    {
        const Vector3 point = s + delta * dist;
        const Vector3 n = pHit->normal();
        printf("%s RAY HIT %08x %08x %08x %08x %08x %08x %08x %08x %04x\n", tag,
            bits(seed), bits(dist),
            bits(point.x), bits(point.y), bits(point.z),
            bits(n.x), bits(n.y), bits(n.z),
            pHit->flags());
    }
    else
    {
        printf("%s RAY MISS %08x %08x\n", tag, bits(seed), bits(dist));
    }
}

// BW-10-03-06: CollisionVisitor that logs every visit (dist bits and the
// candidate's flags) and counts them; the accepting instance stops the
// walk at the first candidate, the rejecting one lets it keep scanning.
class ProbeVisitor : public CollisionVisitor
{
public:
    ProbeVisitor(const char* tag, bool accept) : tag_(tag), accept_(accept), visits_(0) {}
    bool visit(const WorldTriangle& hitTriangle, float dist)
    {
        printf("%s VST %08x %04x\n", tag_, bits(dist), hitTriangle.flags());
        ++visits_;
        return accept_;
    }
    unsigned visits() const { return visits_; }

private:
    const char* tag_;
    bool accept_;
    unsigned visits_;
};

// BSPTree::intersects(triangle, ppHitTriangle) (bsp.cpp:1811-1814).
static void static_probe(const char* tag, BSPTree* tree, const WorldTriangle& t)
{
    const WorldTriangle* pHit = NULL;
    const bool r = tree->intersects(t, &pHit);
    printf("%s STC %s\n", tag, r ? "HIT" : "MISS");
}

// BSPTree::intersects(triangle, translation, pVisitor)
// (bsp.cpp:2013-2016). The VST lines land immediately before this line.
static void sweep_probe(const char* tag, BSPTree* tree, const WorldTriangle& t,
    const Vector3& offset, bool accept)
{
    ProbeVisitor v(tag, accept);
    const bool r = tree->intersects(t, offset, &v);
    printf("%s SWP %s %u\n", tag, r ? "HIT" : "MISS", v.visits());
}

// WorldTriangle::intersects(triangle) (worldtri.cpp:109-196).
static void tri_probe(const char* tag, const WorldTriangle& a, const WorldTriangle& b)
{
    printf("%s TRI %s\n", tag, a.intersects(b) ? "HIT" : "MISS");
}

// WorldTriangle::intersects(triangle, offset) (worldtri.cpp:295-486).
static void tri_sweep_probe(const char* tag, const WorldTriangle& a,
    const WorldTriangle& b, const Vector3& offset)
{
    printf("%s TSW %s\n", tag, a.intersects(b, offset) ? "HIT" : "MISS");
}

static void write_file(const std::string& path, const void* data, size_t len)
{
    FILE* f = fopen(path.c_str(), "wb");
    if (!f || 1 != fwrite(data, len, 1, f))
    {
        fprintf(stderr, "failed to write %s\n", path.c_str());
        exit(2);
    }
    fclose(f);
}

static void write_blob(const std::string& path, BinaryPtr bp)
{
    write_file(path, bp->cdata(), bp->len());
}

int main(int argc, char* argv[])
{
    if (argc != 3)
    {
        fprintf(stderr, "usage: gen_bsp <primitives_path> <out_dir>\n");
        return 1;
    }
    const std::string primPath = argv[1];
    const std::string outDir = argv[2];

    // --- 1. the original constructor builds a tree ---------------------
    // More than MAX_TRIANGLES_PER_NODE (10), so the constructor
    // partitions. The straddler ends up in the root's own run (the
    // reference keeps straddlers with the splitting node), leaving no
    // shared pointers here; the sharing face is covered by legacy.bsp.
    RealWTriangleSet tris;
    WorldTriangle::Flags WATER = 1 << 6;
    // tetra A at the origin
    tris.push_back(WorldTriangle(Vector3(0, 0, 0), Vector3(2, 0, 0), Vector3(0, 2, 0), 0));
    tris.push_back(WorldTriangle(Vector3(0, 0, 0), Vector3(0, 2, 0), Vector3(0, 0, 2), 0));
    tris.push_back(WorldTriangle(Vector3(0, 0, 0), Vector3(0, 0, 2), Vector3(2, 0, 0), 0));
    tris.push_back(WorldTriangle(Vector3(2, 0, 0), Vector3(0, 0, 2), Vector3(0, 2, 0), 0));
    // tetra B at +10 x
    tris.push_back(WorldTriangle(Vector3(10, 0, 0), Vector3(12, 0, 0), Vector3(10, 2, 0), 0));
    tris.push_back(WorldTriangle(Vector3(10, 0, 0), Vector3(10, 2, 0), Vector3(10, 0, 2), 0));
    tris.push_back(WorldTriangle(Vector3(10, 0, 0), Vector3(10, 0, 2), Vector3(12, 0, 0), 0));
    tris.push_back(WorldTriangle(Vector3(12, 0, 0), Vector3(10, 0, 2), Vector3(10, 2, 0), 0));
    // straddles the root split
    tris.push_back(WorldTriangle(Vector3(-1, 0.5f, 0.5f), Vector3(13, 0.5f, 0.5f),
        Vector3(5, 0.5f, 4), 0x0100));
    tris.push_back(WorldTriangle(Vector3(-1, -1, -1), Vector3(-1, 1, -1),
        Vector3(1, 0, -1), WATER));
    tris.push_back(WorldTriangle(Vector3(5, 5, 5), Vector3(6, 5, 5),
        Vector3(5, 6, 5), 0xff));

    BSPTree* built = BSPTreeTool::buildBSP(tris);
    if (!built)
    {
        fprintf(stderr, "buildBSP failed\n");
        return 3;
    }
    write_blob(outDir + "/built.bsp", BSPTreeTool::saveBSPInMemory(built));
    dump_tree("BT", built);
    // Ray battery: down through the straddler then the slant; slant-only;
    // WATER from below (up); the NOT_IN_BSP triangle alone; the seed-0.2
    // clamp misses the straddler hit (~0.2223), 0.25 reaches it; up from
    // below takes the bottom face double-sided.
    ray_probe("BT", built, Vector3(1, 0.5f, 3), Vector3(1, 0.5f, -3), 1.f);
    ray_probe("BT", built, Vector3(0.5f, 1.2f, 3), Vector3(0.5f, 1.2f, -3), 1.f);
    ray_probe("BT", built, Vector3(0.5f, 0.2f, -3), Vector3(0.5f, 0.2f, 1), 1.f);
    ray_probe("BT", built, Vector3(5.2f, 5.2f, 6), Vector3(5.2f, 5.2f, -6), 1.f);
    ray_probe("BT", built, Vector3(1, 0.5f, 3), Vector3(1, 0.5f, -3), 0.45f);
    ray_probe("BT", built, Vector3(1, 0.5f, 3), Vector3(1, 0.5f, -3), 0.4f);
    // Crosses the straddler's plane (y=0.5) at fraction 0.5, point
    // (1, 0.5, 1.5) inside the triangle; the slant would be at 2/3.
    ray_probe("BT", built, Vector3(1, 2, 3), Vector3(1, -1, 0), 1.f);
    ray_probe("BT", built, Vector3(1, 2, 3), Vector3(1, -1, 0), 0.4f);
    ray_probe("BT", built, Vector3(1, 0.5f, -3), Vector3(1, 0.5f, 3), 1.f);
    // Overlap: a broad slab through tetra A, and one far away. Sweeps:
    // down through tetra A (accepting stops at the first candidate,
    // rejecting enumerates them all), and the same prism pushed above
    // the tree.
    static_probe("BT", built, WorldTriangle(Vector3(-1, 0.2f, 2.5f), Vector3(3, 0.4f, 2.5f),
        Vector3(1, 0.6f, -1.5f), 0));
    static_probe("BT", built, WorldTriangle(Vector3(100, 0, 0), Vector3(101, 0, 0),
        Vector3(100, 1, 0), 0));
    WorldTriangle sweepTri(Vector3(-0.5f, -0.5f, 3), Vector3(2.5f, -0.5f, 3),
        Vector3(-0.5f, 2.5f, 3), 0);
    sweep_probe("BT", built, sweepTri, Vector3(0, 0, -4.5f), true);
    sweep_probe("BT", built, sweepTri, Vector3(0, 0, 10), true);
    sweep_probe("BT", built, sweepTri, Vector3(0, 0, -4.5f), false);
    printf("BT RESULT OK\n");

    // --- 2. the crafted legacy blob ------------------------------------
    // Layout per loadBSPLegacy (bsp.cpp:1512-1604): magic | numTriangles |
    // numNodes | maxTriangles (skipped) | 40-byte triangles | nodes
    // (flags u8 | PlaneEq 16 | numRefTris u16 | u16 indices) | user data.
    // Flag byte 0x02 = has front, 0x04 = has back. Pre-order:
    // node0(FB) -> front node1(FB) -> front node2(leaf); the skip walk
    // resolves node3 (back of 1) and node4 (back of 0).
    const uint8_t HAS_FRONT = 0x02;
    const uint8_t HAS_BACK = 0x04;

    std::vector<uint8_t> legacy;
    put_u32(legacy, 0x00505342u | (0u << 24)); // legacy token
    put_u32(legacy, 4);                        // numTriangles
    put_u32(legacy, 5);                        // numNodes
    put_u32(legacy, 0);                        // max triangles (skipped)
    put_tri(legacy, Vector3(0, 0, 0), Vector3(1, 0, 0), Vector3(0, 1, 0), 0x0000);
    put_tri(legacy, Vector3(0, 0, 1), Vector3(1, 0, 1), Vector3(0, 1, 1), 0x0107);
    put_tri(legacy, Vector3(2, 0, 0), Vector3(3, 0, 0), Vector3(2, 1, 0), 0x02ff);
    put_tri(legacy, Vector3(0, 2, 0), Vector3(1, 2, 0), Vector3(0, 3, 0), 0x0301);
    // node0: front+back, refs t0 and t1.
    put_u8(legacy, HAS_FRONT | HAS_BACK);
    put_plane(legacy, PlaneEq(Vector3(1, 0, 0), 0.f));
    put_u16(legacy, 2); put_u16(legacy, 0); put_u16(legacy, 1);
    // node1: front+back, refs t2.
    put_u8(legacy, HAS_FRONT | HAS_BACK);
    put_plane(legacy, PlaneEq(Vector3(0, 1, 0), 1.f));
    put_u16(legacy, 1); put_u16(legacy, 2);
    // node2: leaf, refs t0 (shared with node0).
    put_u8(legacy, 0);
    put_plane(legacy, PlaneEq(Vector3(0, 0, 1), 1.f));
    put_u16(legacy, 1); put_u16(legacy, 0);
    // node3: back of node1 (skip walk), refs t3.
    put_u8(legacy, 0);
    put_plane(legacy, PlaneEq(Vector3(0, 0, 1), 2.f));
    put_u16(legacy, 1); put_u16(legacy, 3);
    // node4: back of node0 (skip walk), refs t1 (shared with node0).
    put_u8(legacy, 0);
    put_plane(legacy, PlaneEq(Vector3(0, 0, 1), 3.f));
    put_u16(legacy, 1); put_u16(legacy, 1);
    put_user(legacy, 1, "abcd");
    put_user(legacy, 2, "z");

    write_file(outDir + "/legacy.bsp", &legacy[0], legacy.size());

    BinaryPtr legacyBin(
        new BinaryBlock(&legacy[0], legacy.size(), "BinaryBlock/GenBSP"));
    BSPTree* legacyTree = BSPTreeTool::loadBSP(legacyBin);
    if (!legacyTree)
    {
        fprintf(stderr, "original loadBSP failed on legacy.bsp\n");
        return 3;
    }
    dump_tree("LG", legacyTree);
    write_blob(outDir + "/legacy_saved.bsp", BSPTreeTool::saveBSPInMemory(legacyTree));
    // All four MISS: the rays are vertical (constant x/y), so they never
    // cross the x=0 (root) or y=1 (node1) partition planes — the walk
    // descends same-side paths and a same-side descent never scans the
    // node's own run. t0/t1 (node0's run) and t2 (node1's run) are never
    // offered even where the rays cross them geometrically (ray 1/4 cross
    // t0's z=0 plane at (0.5,0.5,0)); ray 3 runs in front of node1's
    // plane, so node3's t3 (which it does cross at (0.5,2.5,0)) is never
    // visited either. Engine-faithful walk behavior, pinned as truth.
    ray_probe("LG", legacyTree, Vector3(0.5f, 0.5f, 3), Vector3(0.5f, 0.5f, -3), 1.f);
    ray_probe("LG", legacyTree, Vector3(2.5f, 0.5f, 3), Vector3(2.5f, 0.5f, -3), 1.f);
    ray_probe("LG", legacyTree, Vector3(0.5f, 2.5f, -1), Vector3(0.5f, 2.5f, 4), 1.f);
    ray_probe("LG", legacyTree, Vector3(0.5f, 0.5f, 0.9f), Vector3(0.5f, 0.5f, -1), 1.f);
    // Overlap: the flat fin's z=1 crossing segment covers t1's footprint
    // (x in [0, 0.5] at y=0.5), so the static walk answers HIT. Sweeps:
    // a tilted prism straight up (efficient path), and the fin shoved
    // along its own plane-normal-ish axes (brute path).
    WorldTriangle fin(Vector3(-1, 0.5f, -0.5f), Vector3(2, 0.5f, -0.5f), Vector3(0.5f, 0.5f, 2), 0);
    static_probe("LG", legacyTree, fin);
    WorldTriangle tilt(Vector3(-1, 0.5f, -0.5f), Vector3(2, 0.5f, -0.5f), Vector3(0.5f, 2.5f, 2), 0);
    sweep_probe("LG", legacyTree, tilt, Vector3(0, 0, 3), true);
    sweep_probe("LG", legacyTree, fin, Vector3(0, 0, -3), true);
    sweep_probe("LG", legacyTree, fin, Vector3(0, 3, 0), true);
    printf("LG RESULT OK\n");

    // --- 2b. the WorldTriangle overlap faces, standalone ---------------
    // The tri-tri walk (worldtri.cpp:109-196) and the swept prism
    // (worldtri.cpp:295-486) against a ground triangle: crossing, far,
    // coplanar-degenerate, the Y- and Z-dominant projection picks, the
    // b-only-coplanar reject and the degenerate third distance.
    WorldTriangle wa(Vector3(0, 0, 0), Vector3(2, 0, 0), Vector3(2, 2, 0), 0);
    tri_probe("WT", wa, WorldTriangle(Vector3(1, 0.5f, -1), Vector3(1, 0.5f, 1), Vector3(3, 2, 1), 0));
    tri_probe("WT", wa, WorldTriangle(Vector3(10, 10, -1), Vector3(10, 10, 1), Vector3(12, 12, 1), 0));
    tri_probe("WT", wa, WorldTriangle(Vector3(0.5, 0, 0), Vector3(1.5, 0, 0), Vector3(0.5, 1, 0), 0));
    tri_probe("WT", WorldTriangle(Vector3(0, 0, 0), Vector3(2, 0, 0), Vector3(0, 2, 0), 0),
        WorldTriangle(Vector3(0.5, 0.5f, -1), Vector3(1.5, 0.5f, 1), Vector3(1.5, 1.5f, 1), 0));
    tri_probe("WT", WorldTriangle(Vector3(0, -1, 0), Vector3(0, 1, 0), Vector3(0, 0, -2), 0),
        WorldTriangle(Vector3(-1, 0.2f, 0), Vector3(1, 0.2f, -2), Vector3(-1, 1.2f, -3), 0));
    tri_probe("WT", WorldTriangle(Vector3(0, 0, 0), Vector3(1, 0, 0), Vector3(0, 1, 0), 0),
        WorldTriangle(Vector3(-10, -10, 0.0003f), Vector3(10, 0, -0.0003f), Vector3(0, 10, 0), 0));
    tri_probe("WT", WorldTriangle(Vector3(0, 0, 0), Vector3(1, 0, 0), Vector3(0, 1, 0), 0),
        WorldTriangle(Vector3(0.5, 0.5f, 0), Vector3(1.5, 0.5f, 0), Vector3(0.5, 1.5f, -1), 0));
    // Sweeps: the end cap exactly on the plane (all fractions 1), the
    // start cap coplanar, a prism straddling the plane (brute), a
    // parallel slide (brute), the zero offset against the identical
    // triangle, back-facing caps (the flip branch), the far-cap stop,
    // the Y-dominant projection and the two segment rejects.
    WorldTriangle wb(Vector3(0.5, 0.5f, 1), Vector3(1.5, 0.5f, 1), Vector3(0.5, 1.5f, 1), 0);
    tri_sweep_probe("WT", wa, wb, Vector3(0, 0, -1));
    WorldTriangle wb0(Vector3(0.5, 0.5f, 0), Vector3(1.5, 0.5f, 0), Vector3(0.5, 1.5f, 0), 0);
    tri_sweep_probe("WT", wa, wb0, Vector3(0, 0, -1));
    tri_sweep_probe("WT", wa, WorldTriangle(Vector3(0.3, 0.3f, -1), Vector3(1.5, 0.2f, 1),
        Vector3(0.2, 1.5f, -1), 0), Vector3(0, 0, 0.5f));
    tri_sweep_probe("WT", wa, WorldTriangle(Vector3(0.5, 0.5f, 0.3), Vector3(2, 0.5f, 0.3),
        Vector3(0.5, 2, 0.3), 0), Vector3(1, 0, 0));
    tri_sweep_probe("WT", wa, wa, Vector3(0, 0, 0));
    tri_sweep_probe("WT", wa, WorldTriangle(Vector3(0.5, 0.1f, 0), Vector3(1.5, 0.1f, 1),
        Vector3(0.5, 0.9f, 1), 0), Vector3(1, 0, 0));
    tri_sweep_probe("WT", wa, WorldTriangle(Vector3(-5, 0.2f, 0), Vector3(-6, 0.2f, 1),
        Vector3(-5, 0.8f, 1), 0), Vector3(1, 0, 0));
    tri_sweep_probe("WT", WorldTriangle(Vector3(0, 0, 0), Vector3(1, 0, 1), Vector3(0, 0, -1), 0),
        WorldTriangle(Vector3(0.2, 2, 0.2), Vector3(0.7, 2, 0.2), Vector3(0.2, 2, 0.7), 0),
        Vector3(0, -2.5f, 0));
    tri_sweep_probe("WT", WorldTriangle(Vector3(0, 0, 0), Vector3(1, 0, 0), Vector3(0, 1, 0), 0),
        WorldTriangle(Vector3(5, 5, 1), Vector3(6, 5, 1), Vector3(5, 6, 1), 0), Vector3(0, 0, -1.5f));
    tri_sweep_probe("WT", WorldTriangle(Vector3(0, 0, 0), Vector3(1, 0, 0), Vector3(0, 1, 0), 0),
        WorldTriangle(Vector3(5, 0.4f, 1), Vector3(4, 0.4f, 1), Vector3(5, 1.4f, 1), 0),
        Vector3(0, 0, -1.5f));
    tri_sweep_probe("WT", WorldTriangle(Vector3(0, 0, 0), Vector3(10, 0, 0), Vector3(0, 10, 0), 0),
        WorldTriangle(Vector3(-2, -1, 1), Vector3(-4, -3, 1), Vector3(1, -4, 1), 0),
        Vector3(0, 0, -1.5f));
    printf("WT RESULT OK\n");

    // --- 3. the real asset's bsp2 child (bytes only) --------------------
    // The .primitives container is read with the original BinSection
    // parser, in memory, the way the model loader reaches the bsp2 child.
    // This stale asset does not match the current save layout and the
    // original loader aborts in validate() on it, so it is not loaded
    // here; the port pins that same abort.
    FILE* pf = fopen(primPath.c_str(), "rb");
    if (!pf)
    {
        fprintf(stderr, "cannot open %s\n", primPath.c_str());
        return 2;
    }
    fseek(pf, 0, SEEK_END);
    long primLen = ftell(pf);
    fseek(pf, 0, SEEK_SET);
    std::vector<uint8_t> primRaw(primLen);
    if (fread(&primRaw[0], 1, primLen, pf) != static_cast<size_t>(primLen))
    {
        fprintf(stderr, "short read on %s\n", primPath.c_str());
        return 2;
    }
    fclose(pf);

    BinaryPtr pPrimBin(
        new BinaryBlock(&primRaw[0], primRaw.size(), "BinaryBlock/GenBSP"));
    DataSectionPtr pPrim(new BinSection("unit_cube", pPrimBin));
    DataSectionPtr pBsp2 = pPrim->findChild("bsp2");
    if (!pBsp2 || !pBsp2->asBinary())
    {
        fprintf(stderr, "no bsp2 child\n");
        return 2;
    }
    BinaryPtr raw = pBsp2->asBinary();
    write_file(outDir + "/unit_cube.bsp", raw->cdata(), raw->len());
    printf("UC SOURCE %u bytes\n", raw->len());

    // The same blob, loaded through the original loader: this stale-era
    // asset still satisfies loadBSPCurrent + validate (12 triangles, 2
    // nodes, no shared, no user data), so it doubles as a real-scene
    // fixture the port must replay.
    BSPTree* ucTree = BSPTreeTool::loadBSP(raw);
    if (!ucTree)
    {
        fprintf(stderr, "original loadBSP failed on unit_cube.bsp\n");
        return 3;
    }
    dump_tree("UC", ucTree);
    // Down through the top face; horizontal through a side face; outside
    // the cube entirely.
    ray_probe("UC", ucTree, Vector3(0, 0, 3), Vector3(0, 0, -3), 1.f);
    ray_probe("UC", ucTree, Vector3(3, 0, 0), Vector3(-3, 0, 0), 1.f);
    ray_probe("UC", ucTree, Vector3(5, 5, 6), Vector3(5, 5, -6), 1.f);
    // Sweeps down through the cube, the end cap exactly touching the
    // top plane, and a horizontal slab crossing the cube's sides.
    sweep_probe("UC", ucTree, WorldTriangle(Vector3(-1, -1, 3), Vector3(2, -1, 3),
        Vector3(-1, 2, 3), 0), Vector3(0, 0, -4), true);
    sweep_probe("UC", ucTree, WorldTriangle(Vector3(-1, -1, 2), Vector3(2, -1, 2),
        Vector3(-1, 2, 2), 0), Vector3(0, 0, -1), true);
    static_probe("UC", ucTree, WorldTriangle(Vector3(-2, -0.2f, 0.5), Vector3(2, -0.2f, 0.5),
        Vector3(0, 2, 0.5), 0));
    printf("UC RESULT OK\n");
    delete ucTree;

    delete built;
    delete legacyTree;
    return 0;
}
