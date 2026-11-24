// BW-10-04-05 fixture generator: two portal pairs across three chunks, the
// original ChunkWaypointSet::bind() creating the real connections, and the
// truth probe printout for the cross-chunk waypoint set search.
//
// Compile like gen_waypoints.cpp (links the reference el7 build of
// libwaypoint/libchunk/libresmgr/libmath/libcstdmf; chunk_boundary.cpp comes
// out of libchunk; chunk_waypoint_set_state.cpp out of libwaypoint).
//
// The map: three inside chunks in a line inside the unit box (Chunk::load
// with a NULL section marks them loaded and resets the bounding box to
// [0,1]^3, which is what bind()'s portal scan intersects against).
//   chunk A  x in [0, 1/3]     one square waypoint
//   chunk B  x in [1/3, 2/3]   two waypoints split at z = 1/2 (in-set edge)
//   chunk C  x in [2/3, 1]     one square waypoint
// Portal pairs sit on x = 1/3 (A|B, both directions NON-permissive) and
// x = 2/3 (B|C, both directions permissive). Polygons run CLOCKWISE in the
// x-z plane like the engine's own navpolys.
//
// Truth lines:
//   WROTE / HDR / SET / WP / EDGE         the blob decode (like 04-01)
//   PORTAL <idx> <permissive> <hasChunkItem>
//   CONNTO <set> <seq> <nbrSet> <portal>  connections_ after bind
//   EDGELABEL <set> <absEdge> <nbrSet> <nbrWp> <ex> <ey> <ez>
//   ITERN <set> <wp> <seq> <nbrSet> <nbrWp>  WaypointNeighbourIterator walk
//   XPATHBEGIN/XPATH/XPATHNONE/XPATHEND   chunk-level A* across one portal
//   SETSEARCHBEGIN/SETHOP/SETSEARCHNONE/SETSEARCHEND
//                                         set-level A* (ChunkWPSetState)

#include <cmath>
#include <cstdio>
#include <cstring>
#include <cstdint>
#include <vector>

#include "cstdmf/memory_stream.hpp"
#include "resmgr/binary_block.hpp"
#include "resmgr/bwresource.hpp"
#include "resmgr/xml_section.hpp"

#include "math/vector2.hpp"
#include "math/vector3.hpp"
#include "math/vector4.hpp"

#include "waypoint_generator/waypoint_view.hpp"

#include "waypoint/chunk_waypoint_set_data.hpp"
#include "waypoint/chunk_waypoint_set.hpp"
#include "waypoint/chunk_waypoint_set_state.hpp"
#include "waypoint/waypoint_neighbour_iterator.hpp"
#include "waypoint/chunk_waypoint_state.hpp"
#include "waypoint/navloc.hpp"
#include "waypoint/chunk_navigator.hpp"
#include "waypoint/navigator.hpp"
#include "waypoint/astar.hpp"

#include "chunk/chunk.hpp"
#include "chunk/chunk_boundary.hpp"
#include "chunk/chunk_space.hpp"
#include "chunk/geometry_mapping.hpp"

using namespace std;
using namespace BW;

// ---------------------------------------------------------------------------
// The minimal IWaypointView: four polygons over three sets (one per chunk),
// positions in grid units (gridResolution 1.0, gridMin 0).
// ---------------------------------------------------------------------------
class CrossView : public IWaypointView
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
        float                minHeight, maxHeight;
        int                  set;
        std::vector<Vtx> verts;
    };

    CrossView() : gridRes_(1.0f), gridMin_(0.f, 0.f, 0.f) {}

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
    float getGirth(int) const override { return 0.6f; }
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
    int getSets() const override { return 3; }

    void getVertex(int polygon, int vertex, Vector2& v, int& adjacency,
                   bool& adjToAnotherChunk) const override
    {
        const Vtx& vtx    = polys_[polygon].verts[vertex];
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
    // The empty resource instance: GeometryMapping's settings lookup goes
    // through BWResource and must not assert on the missing singleton.
    BWResource bwResource;
    BWResource::instance().init( BW::vector< BW::string >() );

    CrossView view;
    const float LO = 0.2f, HI = 0.6f; // every waypoint's height band

    // Set 0 = chunk A: the west square. Every edge chunk-boundary.
    view.addPoly(LO, HI, 0,
        {
            { 0.f,      0.f,      0, true }, // CW: +z, +x, -z, -x
            { 0.f,      1.f,      0, true },
            { 1.f / 3.f, 1.f,    0, true },
            { 1.f / 3.f, 0.f,    0, true },
        });

    // Set 1 = chunk B: the middle strip, split at z = 1/2. The shared edge
    // pair runs in opposite directions (B0 north -> B1, B1 south -> B0).
    view.addPoly(LO, HI, 1,
        {
            // poly B0 (global 2), CW
            { 1.f / 3.f, 0.f,      0, true }, // west  -> portal A|B
            { 1.f / 3.f, 0.5f,     3, false }, // north -> B1
            { 2.f / 3.f, 0.5f,     0, true }, // east  -> portal B|C
            { 2.f / 3.f, 0.f,      0, true }, // south
        });
    view.addPoly(LO, HI, 1,
        {
            // poly B1 (global 3), CW
            { 1.f / 3.f, 0.5f,     0, true }, // west  -> portal A|B
            { 1.f / 3.f, 1.f,      0, true }, // north
            { 2.f / 3.f, 1.f,      0, true }, // east  -> portal B|C
            { 2.f / 3.f, 0.5f,     2, false }, // south -> B0
        });

    // Set 2 = chunk C: the east square. Every edge chunk-boundary.
    view.addPoly(LO, HI, 2,
        {
            { 2.f / 3.f, 0.f,      0, true }, // CW
            { 2.f / 3.f, 1.f,      0, true },
            { 1.f,       1.f,      0, true },
            { 1.f,       0.f,      0, true },
        });

    // ---- original writer: version 0 multi-set record ----
    BinaryPtr blob = view.asBinary(Matrix::identity, 0.f);
    const char*  data = blob->cdata();
    const size_t len  = blob->len();

    FILE* out = fopen("./world_navmesh_cross.bin", "wb");
    fwrite(data, 1, len, out);
    fclose(out);
    printf("WROTE ./world_navmesh_cross.bin %zu bytes\n", len);

    // ---- original reader: decode truth, three sets ----
    ChunkWaypointSetData* pSetData[3];
    int base = 1;
    for (int s = 0; s < 3; ++s)
    {
        pSetData[s] = new ChunkWaypointSetData(base);
        ChunkWaypointSetData& setData = *pSetData[s];
        setData.girth(0.6f);
        const char* ptr = data;
        // skip to this set's header the way navmeshFactory walks the record
        for (int skip = 0; skip < s; ++skip)
        {
            int32_t swps  = *(const int32_t*)(ptr + 8);
            int32_t sedge = *(const int32_t*)(ptr + 12);
            ptr += 16;
            ptr += swps * 12;
            ptr += sedge * 12;
        }
        int32_t version = *(const int32_t*)ptr; ptr += 4;
        float   girth   = *(const float*)ptr;   ptr += 4;
        int32_t numWps  = *(const int32_t*)ptr; ptr += 4;
        int32_t numEdges = *(const int32_t*)ptr; ptr += 4;
        printf("HDR%d version %d girth %.6f waypoints %d edges %d\n", s,
               version, girth, numWps, numEdges);
        MF_ASSERT(version == 0);
        ptr = setData.readWaypointSet(ptr, numWps, numEdges);

        const ChunkWaypoints& wps = setData.waypoints();
        printf("SET%d count %d base %d\n", s, int(wps.size()), base);
        for (size_t i = 0; i < wps.size(); ++i)
        {
            const ChunkWaypoint& wp = wps[i];
            printf("WP %d %d minh %.6f maxh %.6f edges %d centre %.6f %.6f\n",
                   s, int(i), wp.minHeight_, wp.maxHeight_,
                   int(wp.edgeCount_), wp.centre_.x, wp.centre_.y);
            for (EdgeIndex e = 0; e < wp.edgeCount_; ++e)
            {
                const ChunkWaypoint::Edge& edge = wp.edges_[e];
                const Vector2&             v    = setData.vertexByIndex(
                    edge.vertexIndex_);
                printf("EDGE %d %d %d %d %.6f %.6f %04X\n", s, int(i),
                       int(e), int(edge.vertexIndex_), v.x, v.y,
                       edge.neighbour_);
            }
        }
        base += int(wps.size());
    }

    // ---- three inside chunks, marked loaded the way the loader does ----
    {
        ChunkSpacePtr pSpace = new ChunkSpace(7);
        XMLSectionPtr pSettings = new XMLSection("space");
        GeometryMapping* pMapping =
            new GeometryMapping(pSpace, Matrix::identity, ".", pSettings);
        BoundingBox localBB(Vector3(-10.f, -10.f, -10.f),
                            Vector3(50.f, 50.f, 50.f));

        Chunk* pChunk[3];
        const char* names[3] = { "chunkA", "chunkB", "chunkC" };
        for (int c = 0; c < 3; ++c)
        {
            pChunk[c] = new Chunk(names[c], pMapping, Matrix::identity, localBB);
            // A NULL section takes the loader's FNF branch: the chunk comes
            // out loaded() with its bounding box reset to [0,1]^3.
            pChunk[c]->load(NULL);
        }

        // ---- mount the sets on their chunks' navigators ----
        ChunkWaypointSetPtr pWay[3];
        for (int s = 0; s < 3; ++s)
        {
            pWay[s] = new ChunkWaypointSet(ChunkWaypointSetDataPtr(pSetData[s]));
            pWay[s]->chunk(pChunk[s]);
            ChunkNavigator::instance(*pChunk[s]).add(pWay[s].get());
            // The reference free list destructs each recycled search state
            // twice (eager ~IntState in free(), then delete in the list
            // destructor), so every A* node drops one set reference too
            // many. Same pin as gen_waypoints.cpp; the algorithm the probes
            // exercise stays untouched.
            for (int pin = 0; pin < 512; ++pin)
            {
                pWay[s]->incRef();
            }
        }

        // ---- portal pairs ----
        // A|B on x = 1/3, NON-permissive in both directions; B|C on x = 2/3,
        // permissive. The 2D points are the (x, z) rectangle corners in
        // clockwise order, which is what findBetterPortal's inside test
        // needs (its cross products must all come out positive).
        PlaneEq planeAB(Vector3(1.f, 0.f, 0.f), 1.f / 3.f);
        PlaneEq planeBC(Vector3(1.f, 0.f, 0.f), 2.f / 3.f);
        BW::vector<Vector3> rectAB, rectBC;
        rectAB.push_back(Vector3(0.30f, 0.90f, 0.f));
        rectAB.push_back(Vector3(0.30f, 0.10f, 0.f));
        rectAB.push_back(Vector3(0.36f, 0.10f, 0.f));
        rectAB.push_back(Vector3(0.36f, 0.90f, 0.f));
        rectBC.push_back(Vector3(0.63f, 0.90f, 0.f));
        rectBC.push_back(Vector3(0.63f, 0.10f, 0.f));
        rectBC.push_back(Vector3(0.70f, 0.10f, 0.f));
        rectBC.push_back(Vector3(0.70f, 0.90f, 0.f));

        ChunkBoundary::Portal* portals[4];
        // 0: chunk A east -> B (non-permissive)
        portals[0] = new ChunkBoundary::Portal(planeAB, Vector3(0, 1, 0),
            rectAB, pMapping, "");
        portals[0]->pChunk = pChunk[1];
        portals[0]->permissive = false;
        // 1: chunk B west -> A (non-permissive)
        portals[1] = new ChunkBoundary::Portal(planeAB, Vector3(0, 1, 0),
            rectAB, pMapping, "");
        portals[1]->pChunk = pChunk[0];
        portals[1]->permissive = false;
        // 2: chunk B east -> C (permissive)
        portals[2] = new ChunkBoundary::Portal(planeBC, Vector3(0, 1, 0),
            rectBC, pMapping, "");
        portals[2]->pChunk = pChunk[2];
        // 3: chunk C west -> B (permissive)
        portals[3] = new ChunkBoundary::Portal(planeBC, Vector3(0, 1, 0),
            rectBC, pMapping, "");
        portals[3]->pChunk = pChunk[1];

        // bind()'s projection reconstructs the far point as
        // origin + uAxis*(uAxis . lwv) + vAxis*(vAxis . lwv); the identity
        // chunks want x and z preserved and the height landed mid-band.
        for (int i = 0; i < 4; ++i)
        {
            portals[i]->uAxis = Vector3(1.f, 0.f, 0.f);
            portals[i]->vAxis = Vector3(0.f, 0.f, 1.f);
            portals[i]->origin = Vector3(0.f, 0.4f, 0.f);
        }

        // Each boundary mounts its portal straight into the bound list via
        // the engine's own mover, so readyToBind's unbound-portal gate (a
        // plain portal there blocks binding) stays clear.
        ChunkBoundary* pBoundA = new ChunkBoundary(planeAB, portals[0]);
        pBoundA->bindPortal(0);
        pChunk[0]->joints().push_back(ChunkBoundaryPtr(pBoundA));

        ChunkBoundary* pBoundBW = new ChunkBoundary(planeAB, portals[1]);
        pBoundBW->bindPortal(0);
        pChunk[1]->joints().push_back(ChunkBoundaryPtr(pBoundBW));
        ChunkBoundary* pBoundBE = new ChunkBoundary(planeBC, portals[2]);
        pBoundBE->bindPortal(0);
        pChunk[1]->joints().push_back(ChunkBoundaryPtr(pBoundBE));

        ChunkBoundary* pBoundC = new ChunkBoundary(planeBC, portals[3]);
        pBoundC->bindPortal(0);
        pChunk[2]->joints().push_back(ChunkBoundaryPtr(pBoundC));

        printf("PORTALS 4\n");
        for (int i = 0; i < 4; ++i)
        {
            printf("PORTAL %d %d %d\n", i, portals[i]->permissive ? 1 : 0,
                   portals[i]->hasChunkItem() ? 1 : 0);
        }

        // ---- the original bind: real connections on both sides ----
        for (int s = 0; s < 3; ++s)
        {
            pWay[s]->bind();
        }

        // set identity helper: pointer -> index
        ChunkWaypointSet* wayPtr[3];
        for (int s = 0; s < 3; ++s) wayPtr[s] = pWay[s].get();
        auto setIdxOf = [&](ChunkWaypointSet* p) -> int {
            for (int s = 0; s < 3; ++s)
                if (p == wayPtr[s]) return s;
            return -1;
        };
        auto portalIdxOf = [&](ChunkBoundary::Portal* p) -> int {
            for (int i = 0; i < 4; ++i)
                if (p == portals[i]) return i;
            return -1;
        };

        // ---- the created connections and edge labels, straight off the
        // reference containers ----
        for (int s = 0; s < 3; ++s)
        {
            int n = 0;
            for (ChunkWaypointConns::const_iterator it = pWay[s]->connectionsBegin();
                 it != pWay[s]->connectionsEnd(); ++it)
            {
                printf("CONNTO %d %d %d %d\n", s, n++,
                       setIdxOf(it->first.get()), portalIdxOf(it->second));
            }
            for (ChunkWaypointEdgeLabels::const_iterator it =
                     pWay[s]->edgeLabelsBegin();
                 it != pWay[s]->edgeLabelsEnd(); ++it)
            {
                const EdgeLabel& l = it->second;
                printf("EDGELABEL %d %d %d %d %.6f %.6f %.6f\n", s,
                       int(it->first), setIdxOf(l.pSet_.get()), int(l.waypoint_),
                       l.entryPoint_.x, l.entryPoint_.y, l.entryPoint_.z);
            }
        }

        // ---- WaypointNeighbourIterator walks, straight off the reference
        // iterator ----
        for (int s = 0; s < 3; ++s)
        {
            ChunkWaypointSetData& setData = *pSetData[s];
            for (size_t i = 0; i < setData.waypoints().size(); ++i)
            {
                printf("ITER %d %d", s, int(i));
                int n = 0;
                for (WaypointNeighbourIterator wni(pWay[s], WaypointIndex(i));
                     !wni.ended(); wni.advance())
                {
                    printf(" %d:%d", setIdxOf(wni.pNeighbourSet().get()),
                           int(wni.neighbourWaypointIndex()));
                    ++n;
                }
                printf(" (count %d)\n", n);
            }
        }

        // ---- chunk-level A* across one portal ----
        {
            struct PathProbe
            {
                const char* name;
                Vector3     src;
                Vector3     dst;
            };
            const PathProbe probes[] = {
                { "AB", Vector3(0.15f, 0.4f, 0.4f), Vector3(0.50f, 0.4f, 0.25f) },
                { "BA", Vector3(0.50f, 0.4f, 0.25f), Vector3(0.15f, 0.4f, 0.4f) },
                { "BC", Vector3(0.50f, 0.4f, 0.25f), Vector3(0.85f, 0.4f, 0.5f) },
                { "CB", Vector3(0.85f, 0.4f, 0.5f), Vector3(0.50f, 0.4f, 0.75f) },
                { "AC", Vector3(0.15f, 0.4f, 0.4f), Vector3(0.85f, 0.4f, 0.5f) },
                { "AA", Vector3(0.15f, 0.4f, 0.4f), Vector3(0.20f, 0.4f, 0.6f) },
            };
            for (size_t p = 0; p < sizeof(probes) / sizeof(probes[0]); ++p)
            {
                int srcChunk, dstChunk;
                if (probes[p].src.x < 1.f / 3.f)       srcChunk = 0;
                else if (probes[p].src.x < 2.f / 3.f)  srcChunk = 1;
                else                                   srcChunk = 2;
                if (probes[p].dst.x < 1.f / 3.f)       dstChunk = 0;
                else if (probes[p].dst.x < 2.f / 3.f)  dstChunk = 1;
                else                                   dstChunk = 2;

                NavLoc src(pChunk[srcChunk], probes[p].src, 0.6f);
                NavLoc dst(pChunk[dstChunk], probes[p].dst, 0.6f);
                printf("XPATHBEGIN %s %d %d\n", probes[p].name,
                       int(src.waypoint()), int(dst.waypoint()));

                ChunkWaypointState srcState(src);
                ChunkWaypointState dstState(dst);
                AStar<ChunkWaypointState,
                      ChunkWaypointState::SearchData>::StateScopedFreeList
                    freeList;
                AStar<ChunkWaypointState, ChunkWaypointState::SearchData>
                    astar(&freeList);
                if (astar.search(srcState, dstState, -1.f))
                {
                    for (const ChunkWaypointState* sSt = astar.first();
                         sSt != NULL; sSt = astar.next())
                    {
                        printf("XPATH %s %.6f %.6f %.6f %d %d\n",
                               probes[p].name, sSt->navLoc().point().x,
                               sSt->navLoc().point().y,
                               sSt->navLoc().point().z,
                               int(sSt->navLoc().waypoint()),
                               setIdxOf(sSt->navLoc().pSet().get()));
                    }
                }
                else
                {
                    printf("XPATHNONE %s\n", probes[p].name);
                }
                printf("XPATHEND %s\n", probes[p].name);
            }
        }

        // ---- set-level A* over the connections ----
        {
            struct SetProbe
            {
                const char* name;
                Vector3     src;
                Vector3     dst;
                int         blockNonPermissive;
            };
            const SetProbe probes[] = {
                // CA0's source z is off 0.5: B's east entries at z 0.25/0.75
                // are equidistant from z 0.5, and the tie would resolve by
                // the reference unordered_map's allocation-dependent order.
                { "AC0", Vector3(0.15f, 0.4f, 0.4f), Vector3(0.85f, 0.4f, 0.5f), 0 },
                { "AC1", Vector3(0.15f, 0.4f, 0.4f), Vector3(0.85f, 0.4f, 0.5f), 1 },
                { "CA0", Vector3(0.85f, 0.4f, 0.6f), Vector3(0.15f, 0.4f, 0.4f), 0 },
                { "AB1", Vector3(0.15f, 0.4f, 0.4f), Vector3(0.50f, 0.4f, 0.25f), 1 },
                { "BC0", Vector3(0.50f, 0.4f, 0.25f), Vector3(0.85f, 0.4f, 0.5f), 0 },
            };
            for (size_t p = 0; p < sizeof(probes) / sizeof(probes[0]); ++p)
            {
                int srcChunk, dstChunk;
                if (probes[p].src.x < 1.f / 3.f)       srcChunk = 0;
                else if (probes[p].src.x < 2.f / 3.f)  srcChunk = 1;
                else                                   srcChunk = 2;
                if (probes[p].dst.x < 1.f / 3.f)       dstChunk = 0;
                else if (probes[p].dst.x < 2.f / 3.f)  dstChunk = 1;
                else                                   dstChunk = 2;

                NavLoc src(pChunk[srcChunk], probes[p].src, 0.6f);
                NavLoc dst(pChunk[dstChunk], probes[p].dst, 0.6f);
                ChunkWPSetState srcState(src);
                ChunkWPSetState dstState(dst);
                srcState.blockNonPermissive(
                    probes[p].blockNonPermissive != 0);
                printf("SETSEARCHBEGIN %s %d %d %d\n", probes[p].name,
                       probes[p].blockNonPermissive, setIdxOf(srcState.pSet().get()),
                       setIdxOf(dstState.pSet().get()));

                AStar<ChunkWPSetState>::StateScopedFreeList setFreeList;
                AStar<ChunkWPSetState> astarSet(&setFreeList);
                if (astarSet.search(srcState, dstState, -1.f))
                {
                    // first()/next() share one cursor, so the path is
                    // collected in a single walk before printing.
                    std::vector<const ChunkWPSetState*> chain;
                    for (const ChunkWPSetState* sSt = astarSet.first();
                         sSt != NULL; sSt = astarSet.next())
                    {
                        chain.push_back(sSt);
                    }
                    for (size_t seq = 0; seq < chain.size(); ++seq)
                    {
                        const ChunkWPSetState* sSt = chain[seq];
                        int hop = -1;
                        if (seq > 0 &&
                            chain[seq - 1]->pSet()->connectionPortal(
                                sSt->pSet()) != NULL)
                        {
                            hop = portalIdxOf(chain[seq - 1]->pSet()->
                                connectionPortal(sSt->pSet()));
                        }
                        printf("SETHOP %s %d %d %.6f %.6f %.6f %d %d %d\n",
                               probes[p].name, int(seq),
                               setIdxOf(sSt->pSet().get()),
                               sSt->position().x, sSt->position().y,
                               sSt->position().z,
                               sSt->passedShellBoundary() ? 1 : 0,
                               sSt->passedActivatedPortal() ? 1 : 0, hop);
                    }
                }
                else
                {
                    printf("SETSEARCHNONE %s\n", probes[p].name);
                }
                printf("SETSEARCHEND %s\n", probes[p].name);
            }
        }

        // ---- the original navigator's findPath: the unreachable face ----
        {
            struct FindProbe
            {
                const char* name;
                Vector3     src;
                Vector3     dst;
                int         blockNonPermissive;
            };
            const FindProbe probes[] = {
                // AC0 then its exact repeat exercises the cache-hit arm
                // (findWayPath + moveStateAlongPath).
                { "AC0",  Vector3(0.15f, 0.4f, 0.4f), Vector3(0.85f, 0.4f, 0.5f), 0 },
                { "AC0R", Vector3(0.15f, 0.4f, 0.4f), Vector3(0.85f, 0.4f, 0.5f), 0 },
                { "AC1",  Vector3(0.15f, 0.4f, 0.4f), Vector3(0.85f, 0.4f, 0.5f), 1 },
                { "CA0",  Vector3(0.85f, 0.4f, 0.6f), Vector3(0.15f, 0.4f, 0.4f), 0 },
                { "CA1",  Vector3(0.85f, 0.4f, 0.6f), Vector3(0.15f, 0.4f, 0.4f), 1 },
                { "AB0",  Vector3(0.15f, 0.4f, 0.4f), Vector3(0.50f, 0.4f, 0.25f), 0 },
                { "AB1",  Vector3(0.15f, 0.4f, 0.4f), Vector3(0.50f, 0.4f, 0.25f), 1 },
                { "CB0",  Vector3(0.85f, 0.4f, 0.5f), Vector3(0.50f, 0.4f, 0.75f), 0 },
                { "CB1",  Vector3(0.85f, 0.4f, 0.5f), Vector3(0.50f, 0.4f, 0.75f), 1 },
                { "AA0",  Vector3(0.15f, 0.4f, 0.4f), Vector3(0.20f, 0.4f, 0.6f), 0 },
            };
            Navigator navigator;
            for (size_t p = 0; p < sizeof(probes) / sizeof(probes[0]); ++p)
            {
                int srcChunk, dstChunk;
                if (probes[p].src.x < 1.f / 3.f)       srcChunk = 0;
                else if (probes[p].src.x < 2.f / 3.f)  srcChunk = 1;
                else                                   srcChunk = 2;
                if (probes[p].dst.x < 1.f / 3.f)       dstChunk = 0;
                else if (probes[p].dst.x < 2.f / 3.f)  dstChunk = 1;
                else                                   dstChunk = 2;

                NavLoc src(pChunk[srcChunk], probes[p].src, 0.6f);
                NavLoc dst(pChunk[dstChunk], probes[p].dst, 0.6f);
                printf("FPBEGIN %s %d %d %d\n", probes[p].name,
                       int(src.waypoint()), int(dst.waypoint()),
                       probes[p].blockNonPermissive);

                NavLoc next;
                bool passedActivatedPortal = false;
                if (navigator.findPath(src, dst, 1000000.f,
                        probes[p].blockNonPermissive != 0, next,
                        passedActivatedPortal))
                {
                    printf("FPOK %s %.6f %.6f %.6f %d %d %d\n",
                           probes[p].name,
                           next.point().x, next.point().y, next.point().z,
                           int(next.waypoint()),
                           setIdxOf(next.pSet().get()),
                           passedActivatedPortal ? 1 : 0);
                }
                else
                {
                    printf("FPNONE %s\n", probes[p].name);
                }
                printf("FPEND %s\n", probes[p].name);
            }
        }
    }

    printf("RESULT OK\n");

    return 0;
}
