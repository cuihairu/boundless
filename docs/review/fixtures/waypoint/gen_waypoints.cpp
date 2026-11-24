// BW-10-04-01 fixture generator: the original navgen writer/reader pair
// produces the sample worldNavmesh blob and the truth probe printout.
//
// Compile (cmake-entitydef flags.make flags): links the reference el7 build
// of libwaypoint/libchunk/libresmgr/libmath/libcstdmf. The blob is the
// original IWaypointView::asBinary output (version 0 navpoly sets, exactly
// what ChunkWaypointSetData::navmeshFactory reads); the truth side feeds the
// same bytes through the original ChunkWaypointSetData::readWaypointSet and
// prints probe lines (WP/EDGE/RESULT).
//
// The old graph: set 0 (girth 0.6) chains three polygons A-B-C with shared
// corners (vertex dedup), one chunk-boundary edge (65535) and three vista
// edges (negative adjacency -> top-bit flags); set 1 (girth 1.0) holds a
// lone quad with two boundary and two vista edges. The polygons run
// CLOCKWISE in the x-z plane, like the engine's own navpolys — the
// reference containsProjection rejects interior points of CCW polygons.

#include <cmath>
#include <cstdio>
#include <cstring>
#include <cstdint>
#include <vector>

#include "cstdmf/memory_stream.hpp"
#include "resmgr/binary_block.hpp"

#include "math/vector2.hpp"
#include "math/vector3.hpp"
#include "math/vector4.hpp"

#include "waypoint_generator/waypoint_view.hpp"

#include "waypoint/chunk_waypoint_set_data.hpp"

using namespace std;
using namespace BW;

// ---------------------------------------------------------------------------
// The minimal IWaypointView: four polygons over two girth sets, positions in
// grid units (gridResolution 1.0, gridMin 0).
// ---------------------------------------------------------------------------
class FixtureView : public IWaypointView
{
public:
    struct Vtx
    {
        float x, z;
        int   adjacency;          // 1-based global poly id, <=0 vista
        bool  adjToAnotherChunk;
    };
    struct Poly
    {
        float           minHeight, maxHeight;
        int             set;
        std::vector<Vtx> verts;
    };

    FixtureView() : gridRes_(1.0f), gridMin_(0.f, 0.f, 0.f) {}

    uint32_t gridX() const override { return 16; }
    uint32_t gridZ() const override { return 16; }
    uint8_t  gridMask(int, int) const override { return 0; }
    Vector3  gridMin() const override { return gridMin_; }
    Vector3  gridMax() const override
    {
        return Vector3(gridMin_.x + gridX() * gridRes_, 0.f,
                       gridMin_.z + gridZ() * gridRes_);
    }
    float gridResolution() const override { return gridRes_; }

    int   getPolygonCount() const override { return int(polys_.size()); }
    float getGirth(int set) const override { return set == 0 ? 0.6f : 1.0f; }
    int   getVertexCount(int polygon) const override
    {
        return int(polys_[polygon].verts.size());
    }
    float getMinHeight(int polygon) const override
    {
        return polys_[polygon].minHeight;
    }
    float getMaxHeight(int polygon) const override
    {
        return polys_[polygon].maxHeight;
    }
    int getSet(int polygon) const override { return polys_[polygon].set; }
    int getSets() const override { return 2; }

    void getVertex(int polygon, int vertex, Vector2& v, int& adjacency,
                   bool& adjToAnotherChunk) const override
    {
        const Vtx& vtx = polys_[polygon].verts[vertex];
        v                 = Vector2(vtx.x, vtx.z);
        adjacency         = vtx.adjacency;
        adjToAnotherChunk = vtx.adjToAnotherChunk;
    }

    BW::vector<int> findWaypoints(const Vector3&, float) const override
    {
        return BW::vector<int>();
    }

    const BW::string& identifier() const override { return id_; }

    void addPoly(float minHeight, float maxHeight, int set,
                 const std::vector<Vtx>& verts)
    {
        polys_.push_back(Poly{ minHeight, maxHeight, set, verts });
    }

private:
    std::vector<Poly> polys_;
    float        gridRes_;
    Vector3      gridMin_;
    BW::string   id_;
};

static void putF(std::vector<uint8_t>& b, float f)
{
    const uint8_t* q = reinterpret_cast<const uint8_t*>(&f);
    b.insert(b.end(), q, q + 4);
}
static void putI(std::vector<uint8_t>& b, int32_t i)
{
    const uint8_t* q = reinterpret_cast<const uint8_t*>(&i);
    b.insert(b.end(), q, q + 4);
}

int main()
{
    FixtureView view;

    // Set 0 (girth 0.6): the A-B-C chain with shared corners.
    // Adjacency is the 1-based GLOBAL polygon id; the writer remaps it to
    // the 0-based local index (pos.indexMap[p+1] = pos.count++).
    // Vertices run CLOCKWISE in the x-z plane — the reference
    // containsProjection rejects interior points of CCW polygons, so the
    // writer's order is reversed together with the edge adjacency lists.
    view.addPoly(0.f, 1.f, 0,
        {
            // poly A (global 1), CW
            { 0.f, 2.f, 0, true },   // chunk boundary (65535)
            { 2.f, 2.f, -5, false }, // vista, cover bits from ~5
            { 2.f, 0.f, 2, false },  // -> B (shared corner with B)
            { 0.f, 0.f, 2, false },  // -> B
        });
    view.addPoly(1.f, 1.f, 0,
        {
            // poly B (global 2), CW
            { 2.f, 2.f, -2, false }, // vista
            { 2.f, 2.f, 1, false },  // -> A (shared corner with A)
            { 4.f, 0.f, 3, false },  // -> C (shared corner with C)
            { 2.f, 0.f, 3, false },  // -> C
        });
    view.addPoly(2.f, 4.f, 0,
        {
            // poly C (global 3), CW
            { 4.f, 2.f, 0, true },   // chunk boundary
            { 4.f, 0.f, -7, false }, // vista
            { 2.f, 2.f, 2, false },  // -> B (shared vertex)
        });

    // Set 1 (girth 1.0): the lone quad D (global 4), CW.
    view.addPoly(0.f, 2.f, 1,
        {
            { 10.f, 12.f, -3, false },// vista
            { 12.f, 12.f, 0, true },  // chunk boundary
            { 12.f, 10.f, -1, false },// vista
            { 10.f, 10.f, 0, true },  // chunk boundary
        });

    // ---- original writer: version 0 multi-set record ----
    BinaryPtr blob = view.asBinary(Matrix::identity, 0.f);
    const char*  data = blob->cdata();
    const size_t len  = blob->len();

    FILE* out = fopen("./world_navmesh.bin", "wb");
    fwrite(data, 1, len, out);
    fclose(out);
    printf("WROTE ./world_navmesh.bin %zu bytes\n", len);

    // ---- original reader: decode truth ----
    {
        // set 0: base index 1 (navmeshFactory: "yes, it starts from 1")
        // heap-allocated: the reference dtor is private (SafeReferenceCount)
        ChunkWaypointSetData* pSetA = new ChunkWaypointSetData(1);
        ChunkWaypointSetData& setA = *pSetA;
        setA.girth(view.getGirth(0));
        const char* ptr = data;
        // walk the set-0 header the way navmeshFactory does
        int32_t aVersion = *(const int32_t*)ptr; ptr += 4;
        float   aGirth   = *(const float*)ptr;   ptr += 4;
        int32_t numWps   = *(const int32_t*)ptr; ptr += 4;
        int32_t numEdges = *(const int32_t*)ptr; ptr += 4;
        printf("HDR0 version %d girth %.6f waypoints %d edges %d\n", aVersion,
               aGirth, numWps, numEdges);
        MF_ASSERT(aVersion == 0);
        ptr = setA.readWaypointSet(ptr, numWps, numEdges);

        const ChunkWaypoints& wps = setA.waypoints();
        printf("SET0 count %d base 1\n", int(wps.size()));
        for (size_t i = 0; i < wps.size(); ++i)
        {
            const ChunkWaypoint& wp = wps[i];
            printf("WP %d minh %.6f maxh %.6f edges %d centre %.6f %.6f\n",
                   int(i), wp.minHeight_, wp.maxHeight_, int(wp.edgeCount_),
                   wp.centre_.x, wp.centre_.y);
            for (EdgeIndex e = 0; e < wp.edgeCount_; ++e)
            {
                const ChunkWaypoint::Edge& edge = wp.edges_[e];
                const Vector2&             v    = setA.vertexByIndex(
                    edge.vertexIndex_);
                printf("EDGE %d %d %d %.6f %.6f %04X\n", int(i), int(e),
                       int(edge.vertexIndex_), v.x, v.y, edge.neighbour_);
            }
        }

        // set 1: base index accumulates (1 + 3 waypoints)
        ChunkWaypointSetData* pSetB = new ChunkWaypointSetData(1 + int(wps.size()));
        ChunkWaypointSetData& setB = *pSetB;
        setB.girth(view.getGirth(1));
        aVersion = *(const int32_t*)ptr; ptr += 4;
        aGirth   = *(const float*)ptr;   ptr += 4;
        numWps   = *(const int32_t*)ptr; ptr += 4;
        numEdges = *(const int32_t*)ptr; ptr += 4;
        printf("HDR1 version %d girth %.6f waypoints %d edges %d\n", aVersion,
               aGirth, numWps, numEdges);
        MF_ASSERT(aVersion == 0);
        ptr = setB.readWaypointSet(ptr, numWps, numEdges);

        const ChunkWaypoints& wpsB = setB.waypoints();
        printf("SET1 count %d base %d\n", int(wpsB.size()), 1 + int(wps.size()));
        for (size_t i = 0; i < wpsB.size(); ++i)
        {
            const ChunkWaypoint& wp = wpsB[i];
            printf("WP %d minh %.6f maxh %.6f edges %d centre %.6f %.6f\n",
                   int(i), wp.minHeight_, wp.maxHeight_, int(wp.edgeCount_),
                   wp.centre_.x, wp.centre_.y);
            for (EdgeIndex e = 0; e < wp.edgeCount_; ++e)
            {
                const ChunkWaypoint::Edge& edge = wp.edges_[e];
                const Vector2&             v    = setB.vertexByIndex(
                    edge.vertexIndex_);
                printf("EDGE %d %d %d %.6f %.6f %04X\n", int(i), int(e),
                       int(edge.vertexIndex_), v.x, v.y, edge.neighbour_);
            }
        }

        // adjacency decode probes straight off the reference accessors
        // (A: 0 boundary, 1 vista, 2-3 the B links; C: 0 boundary, 1 vista,
        // 2 the B link)
        const ChunkWaypoint& wpA = wps[0];
        printf("ADJ %d %d %d\n", 0, 2,
               int(wpA.edges_[2].neighbouringWaypoint()));
        printf("ADJ %d %d %d\n", 0, 0,
               int(wpA.edges_[0].neighbouringWaypoint()));
        printf("CHUNKBOUND %d %d %d\n", 0, 0,
               int(wpA.edges_[0].adjacentToChunk()));
        printf("VISTA %04X %04X %04X\n", wpA.edges_[1].neighbour_,
               wps[1].edges_[0].neighbour_, wps[2].edges_[1].neighbour_);

        // ---- BW-10-04-02: locate & project probes, straight off the
        // reference objects ----
        // CONTAINS <wp> <x> <y> <z> <0/1> — the height range and the
        // projection containment in one call.
        printf("CONTAINS 0 0 1 1 1 %d\n", wps[0].contains(setA, GeoSpaceVector3(1.f, 1.f, 1.f)) ? 1 : 0);
        printf("CONTAINS 0 0 1 0.5 1 %d\n", wps[0].contains(setA, GeoSpaceVector3(1.f, 0.5f, 1.f)) ? 1 : 0);
        printf("CONTAINS 0 0 1 1.95 1 %d\n", wps[0].contains(setA, GeoSpaceVector3(1.f, 1.95f, 1.f)) ? 1 : 0);
        printf("CONTAINS 0 0 1 2.05 1 %d\n", wps[0].contains(setA, GeoSpaceVector3(1.f, 2.05f, 1.f)) ? 1 : 0);
        printf("CONTAINS 0 1 2.5 1 1 %d\n", wps[1].contains(setA, GeoSpaceVector3(2.5f, 1.f, 1.f)) ? 1 : 0);
        printf("CONTAINS 0 2 2.5 2.95 1 %d\n", wps[2].contains(setA, GeoSpaceVector3(2.5f, 2.95f, 1.f)) ? 1 : 0);
        printf("CONTAINS 0 2 3.5 3 1 %d\n", wps[2].contains(setA, GeoSpaceVector3(3.5f, 3.f, 1.f)) ? 1 : 0);
        // on the boundary edge (x = 0): the -0.01 fudge keeps it in.
        printf("CONTAINS 0 0 0 0.5 1 %d\n", wps[0].contains(setA, GeoSpaceVector3(0.f, 0.5f, 1.f)) ? 1 : 0);
        // a point outside every projection.
        printf("CONTAINS 0 0 1 5 1 %d\n", wps[0].contains(setA, GeoSpaceVector3(1.f, 5.f, 1.f)) ? 1 : 0);
        // set 1's quad D.
        printf("CONTAINS 1 0 11 1 11 %d\n", wpsB[0].contains(setB, GeoSpaceVector3(11.f, 1.f, 11.f)) ? 1 : 0);
        printf("CONTAINS 1 0 13 1 11 %d\n", wpsB[0].contains(setB, GeoSpaceVector3(13.f, 1.f, 11.f)) ? 1 : 0);

        // CONTAINSPROJ <wp> <x> <z> <0/1> — the projection test alone.
        printf("CONTAINSPROJ 0 0 1 1 %d\n", wps[0].containsProjection(setA, GeoSpaceVector3(1.f, 0.f, 1.f)) ? 1 : 0);
        printf("CONTAINSPROJ 0 0 0.5 0.5 %d\n", wps[0].containsProjection(setA, GeoSpaceVector3(0.5f, 0.f, 0.5f)) ? 1 : 0);
        printf("CONTAINSPROJ 0 0 3 1 %d\n", wps[0].containsProjection(setA, GeoSpaceVector3(3.f, 0.f, 1.f)) ? 1 : 0);

        // FIND <x> <y> <z> <idx> — the containing waypoint, or -1.
        printf("FIND 1 1 1 %d\n", setA.find(GeoSpaceVector3(1.f, 1.f, 1.f)));
        printf("FIND 1 0.5 1 %d\n", setA.find(GeoSpaceVector3(1.f, 0.5f, 1.f)));
        printf("FIND 1 1.95 1 %d\n", setA.find(GeoSpaceVector3(1.f, 1.95f, 1.f)));
        printf("FIND 1 2.05 1 %d\n", setA.find(GeoSpaceVector3(1.f, 2.05f, 1.f)));
        printf("FIND 2.5 1 1 %d\n", setA.find(GeoSpaceVector3(2.5f, 1.f, 1.f)));
        printf("FIND 2.5 2.95 1 %d\n", setA.find(GeoSpaceVector3(2.5f, 2.95f, 1.f)));
        printf("FIND 3.5 3 1 %d\n", setA.find(GeoSpaceVector3(3.5f, 3.f, 1.f)));
        printf("FIND 0 0.5 1 %d\n", setA.find(GeoSpaceVector3(0.f, 0.5f, 1.f)));
        printf("FIND 1 5 1 %d\n", setA.find(GeoSpaceVector3(1.f, 5.f, 1.f)));

        // FINDIGN <x> <y> <z> <idx> — ignoreHeight true: the best height
        // fit among the projection-containing waypoints.
        printf("FINDIGN 1 5 1 %d\n", setA.find(GeoSpaceVector3(1.f, 5.f, 1.f), true));
        printf("FINDIGN 2.5 5 1 %d\n", setA.find(GeoSpaceVector3(2.5f, 5.f, 1.f), true));
        printf("FINDIGN 2.5 1.05 1 %d\n", setA.find(GeoSpaceVector3(2.5f, 1.05f, 1.f), true));
        printf("FINDIGN 1 1 1 %d\n", setA.find(GeoSpaceVector3(1.f, 1.f, 1.f), true));


        // ---- BW-10-04-03: the girth match window ----
        // The set's girth is the only thing ChunkNavigator::find filters on
        // before searching (almostEqual, epsilon 0.0004f). Probe the
        // matcher straight off the reference helper and show the second set
        // (girth 1.0) answering the same point that set 0 (girth 0.6) owns.
        printf("GIRTHMATCH %.3f %.3f %d\n", 0.6f, 0.6f, almostEqual(0.6f, 0.6f));
        printf("GIRTHMATCH %.3f %.3f %d\n", 0.6f, 1.0f, almostEqual(0.6f, 1.0f));
        printf("GIRTHMATCH %.3f %.3f %d\n", 0.6f, 0.6004f, almostEqual(0.6f, 0.6004f));
        printf("GIRTHMATCH %.3f %.3f %d\n", 0.6f, 0.6003f, almostEqual(0.6f, 0.6003f));
        printf("GIRTHMATCH %.3f %.3f %d\n", 1.0f, 1.0f, almostEqual(1.0f, 1.0f));

        // The girth filter picks the set: the same point over A's footprint
        // answers in set 0 at girth 0.6 and answers -1 at girth 1.0, where
        // only the lone quad D (10..12) is loaded.
        ChunkWaypointSetData* pSets[2] = { pSetA, pSetB };
        const float girths[2] = { 0.6f, 1.0f };
        const float probeX = 1.f;
        const float probeY = 1.f;
        const float probeZ = 1.f;
        for (int slot = 0; slot < 2; ++slot)
        {
            for (int other = 0; other < 2; ++other)
            {
                printf("GIRTHFIND %.3f %.3f %.6f %.6f %.6f %d %d\n",
                       girths[slot], girths[other], probeX, probeY, probeZ,
                       almostEqual(girths[slot], girths[other]),
                       other == slot
                           ? pSets[other]->find(GeoSpaceVector3(probeX, probeY, probeZ))
                           : -99);
            }
        }

        // And the ignoreHeight form of the same search at the doorway
        // height, where nothing is in the height band but the projection
        // still matches.
        for (int slot = 0; slot < 2; ++slot)
        {
            printf("GIRTHFINDIGN %.3f %.6f %.6f %.6f %d\n",
                   girths[slot], 2.5f, 5.f, 1.f,
                   pSets[slot]->find(GeoSpaceVector3(2.5f, 5.f, 1.f), true));
        }

        printf("RESULT OK\n");
    }

    return 0;
}
