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
//   DOK/DPNONE                            findPath under a maxSearchDistance
//                                         grid straddling the two original
//                                         thresholds (the A* filter
//                                         distanceToGoal(start) > max and
//                                         the in-set clamp max > gridSize;
//                                         fresh Navigator per probe)
//   DCACHEOK/DCACHENONE                   a changed limit on one Navigator
//                                         (the shell-boundary bypass makes
//                                         every call re-search)
//   ECLEAR/EOK/ENONE                      the cache lifecycle on ONE
//                                         Navigator: serve, invalidate,
//                                         re-path (E1-E8), then the same
//                                         query over a rebuilt resource
//                                         (EHDR/EWP decode it, E10)

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

        // set identity helper: pointer -> index (slot 3 fills in with the
        // rebuilt-B resource at the E section)
        ChunkWaypointSet* wayPtr[4];
        for (int s = 0; s < 3; ++s) wayPtr[s] = pWay[s].get();
        wayPtr[3] = NULL;
        auto setIdxOf = [&](ChunkWaypointSet* p) -> int {
            for (int s = 0; s < 4; ++s)
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

        // ---- the original navigator's maxSearchDistance: the limit face ----
        // Limits straddling the two original thresholds: the A* filter
        // "distanceToGoal(start) > maxDistance" (astar.ipp:125-131) and the
        // in-set clamp "maxSearchDistance > src.gridSize()" (navigator.cpp
        // :455-460, the probes carry a 0.6 grid). A fresh Navigator per
        // probe keeps the cache out of the picture.
        //
        // Pairs and what binds them (x-z flips sit exactly on the pair
        // distance; y is the located 0.41 on both ends):
        //   AA  same set, same waypoint 0 — the start isGoal's the goal-only
        //       / waypoint-equal test outright, no expansion, any limit OK.
        //   AB  flip at |src->dst| = sqrt(0.145)  = 0.380789 (set-level goal
        //       candidate carries the dst point as its position).
        //   AC  flip at sqrt(0.50) = 0.707107.
        //   CA  flip at sqrt(0.53) = 0.728010.
        //   BC  flip at sqrt(0.185) = 0.430116.
        //   BB  same set across the z = 1/2 in-set edge: only the in-set
        //       filter binds (candidate = the crossing point, est. 0.314).
        //   AE  dst just past the A|B portal: the set level clears at
        //       ~0.253/0.254 but the in-set crossing (~0.266) still trips a
        //       small limit — the in-set filter is the binder.
        {
            printf("GRID %.4f\n", pChunk[0]->space()->gridSize());

            struct DistProbe
            {
                const char* name;
                int         srcChunk;
                int         dstChunk;
                Vector3     src;
                Vector3     dst;
                const float* limits;
                size_t       limitCount;
            };
            static const float LIM_AA[] =
                { -1.f, 0.f, 0.001f, 0.05f, 0.6f, 2.f };
            static const float LIM_AB[] =
                { -1.f, 0.f, 0.30f, 0.35f, 0.3807f, 0.3808f, 0.381f, 0.4f, 0.6f, 2.f };
            static const float LIM_AC[] =
                { -1.f, 0.f, 0.5f, 0.65f, 0.70f, 0.7071f, 0.7072f, 0.71f, 0.8f, 2.f };
            static const float LIM_CA[] =
                { -1.f, 0.f, 0.70f, 0.7279f, 0.7280f, 0.7281f, 0.73f, 0.8f, 2.f };
            static const float LIM_BC[] =
                { -1.f, 0.f, 0.35f, 0.42f, 0.4300f, 0.4302f, 0.44f, 2.f };
            static const float LIM_BB[] =
                { -1.f, 0.f, 0.25f, 0.30f, 0.313f, 0.314f, 0.3141f, 0.315f,
                  0.32f, 0.5f, 2.f };
            static const float LIM_AE[] =
                { -1.f, 0.f, 0.254f, 0.26f, 0.265f, 0.2656f, 0.2657f, 0.267f,
                  0.27f, 0.4f, 2.f };
            const DistProbe probes[] = {
                { "AA", 0, 0, Vector3(0.15f, 0.4f, 0.4f), Vector3(0.20f, 0.4f, 0.6f),
                  LIM_AA, sizeof(LIM_AA) / sizeof(LIM_AA[0]) },
                { "AB", 0, 1, Vector3(0.15f, 0.4f, 0.4f), Vector3(0.50f, 0.4f, 0.25f),
                  LIM_AB, sizeof(LIM_AB) / sizeof(LIM_AB[0]) },
                { "AC", 0, 2, Vector3(0.15f, 0.4f, 0.4f), Vector3(0.85f, 0.4f, 0.5f),
                  LIM_AC, sizeof(LIM_AC) / sizeof(LIM_AC[0]) },
                { "CA", 2, 0, Vector3(0.85f, 0.4f, 0.6f), Vector3(0.15f, 0.4f, 0.4f),
                  LIM_CA, sizeof(LIM_CA) / sizeof(LIM_CA[0]) },
                { "BC", 1, 2, Vector3(0.50f, 0.4f, 0.25f), Vector3(0.85f, 0.4f, 0.5f),
                  LIM_BC, sizeof(LIM_BC) / sizeof(LIM_BC[0]) },
                { "BB", 1, 1, Vector3(0.50f, 0.4f, 0.25f), Vector3(0.50f, 0.4f, 0.75f),
                  LIM_BB, sizeof(LIM_BB) / sizeof(LIM_BB[0]) },
                { "AE", 0, 1, Vector3(0.15f, 0.4f, 0.4f), Vector3(0.40f, 0.4f, 0.36f),
                  LIM_AE, sizeof(LIM_AE) / sizeof(LIM_AE[0]) },
            };
            for (size_t p = 0; p < sizeof(probes) / sizeof(probes[0]); ++p)
            {
                for (size_t m = 0; m < probes[p].limitCount; ++m)
                {
                    const float limit = probes[p].limits[m];
                    NavLoc src(pChunk[probes[p].srcChunk], probes[p].src, 0.6f);
                    NavLoc dst(pChunk[probes[p].dstChunk], probes[p].dst, 0.6f);
                    Navigator navigator;
                    NavLoc  next;
                    bool    passedActivatedPortal = false;

                    if (navigator.findPath(src, dst, limit, false, next,
                            passedActivatedPortal))
                    {
                        printf("DOK %s %.4f %.6f %.6f %.6f %d %d %d\n",
                               probes[p].name, limit,
                               next.point().x, next.point().y,
                               next.point().z, int(next.waypoint()),
                               setIdxOf(next.pSet().get()),
                               passedActivatedPortal ? 1 : 0);
                    }
                    else
                    {
                        printf("DPNONE %s %.4f\n", probes[p].name, limit);
                    }
                }
            }
        }

        // ---- a changed limit on the same Navigator: the set path never
        // serves again ----
        // Every set path in this world crosses a shell boundary (all chunks
        // are inside, so every cross-set hop sets the flag), and the
        // reference bypasses the set cache whenever it did. Each call below
        // therefore re-searches, and a limit tighter than the pair distance
        // fails even on the Navigator that just answered unlimited.
        {
            const Vector3 src(0.15f, 0.4f, 0.4f);
            const Vector3 dst(0.85f, 0.4f, 0.5f);
            Navigator navigator;
            const float limits[] = { -1.f, 0.05f, 0.00f, 0.30f };
            for (size_t m = 0; m < sizeof(limits) / sizeof(limits[0]); ++m)
            {
                NavLoc  from(pChunk[0], src, 0.6f);
                NavLoc  to(pChunk[2], dst, 0.6f);
                NavLoc  next;
                bool    passedActivatedPortal = false;
                if (navigator.findPath(from, to, limits[m], false, next,
                        passedActivatedPortal))
                {
                    printf("DCACHEOK %d %.4f %.6f %.6f %.6f\n", int(m),
                           limits[m], next.point().x, next.point().y,
                           next.point().z);
                }
                else
                {
                    printf("DCACHENONE %d %.4f\n", int(m), limits[m]);
                }
            }
        }

        // ---- the cache lifecycle on one Navigator: serve, invalidate,
        // re-path ----
        // Rows E1-E8 share ONE Navigator; everything but E7 is the same-set
        // BB query (wp0 (0.5,0.4,0.25) -> wp1 (0.5,0.4,0.75), fresh-search
        // threshold 0.3140|0.3141 per the D sweep above):
        //   E1 -1    OK    the way path lands in the cache
        //   E2 0.25  OK    the cache serves; a fresh search would refuse
        //                  (DPNONE BB 0.2500 above) — cache hits skip the
        //                  maxSearchDistance filter entirely
        //   E3 0.25  NONE  after clearCachedWayPath(): the public
        //                  invalidation face forces a fresh failing search
        //   E4 -1    OK    searched and cached again
        //   E5 0.25  OK    dst (0.5,0.4,7/10): same set + waypoint, new
        //                  point — waypointsEqual keys on set identity and
        //                  waypoint index only, so the cache serves and
        //                  next is the new dst itself
        //   E6 -1    OK    dst (0.5,0.4,0.3) lands in wp0: matches()
        //                  refuses the changed destination and the fresh
        //                  search re-paths (src waypoint == dst waypoint)
        //   E7 -1    OK    the AC cross-set query re-searches the set path
        //                  and its shell-bypass arm wipes BOTH caches
        //   E8 0.25  NONE  the portal-crossing query invalidated the way
        //                  path cache — the same query E2 answered OK
        //   E9 -1    OK    re-prime: the way path is back in the cache
        //   E10 0.25 OK    the resource changed under the Navigator: the
        //                  rebuilt B (no z = 1/2 split, one waypoint) is a
        //                  different set object, so the stale cached states
        //                  never match (waypointsEqual compares set
        //                  pointers) and the search re-runs over the new
        //                  resource — the same 0.25 limit that fails the
        //                  old geometry now succeeds, and the set column
        //                  answers 3 (a served-stale answer would say 1)
        Navigator eNavigator;
        {
            struct EProbe
            {
                const char* name;
                int         srcChunk;
                int         dstChunk;
                Vector3     src;
                Vector3     dst;
                float       limit;
            };
            const EProbe steps[] = {
                { "E1", 1, 1, Vector3(0.5f, 0.4f, 0.25f),
                  Vector3(0.5f, 0.4f, 0.75f), -1.f },
                { "E2", 1, 1, Vector3(0.5f, 0.4f, 0.25f),
                  Vector3(0.5f, 0.4f, 0.75f), 0.25f },
                { "E3", 1, 1, Vector3(0.5f, 0.4f, 0.25f),
                  Vector3(0.5f, 0.4f, 0.75f), 0.25f },
                { "E4", 1, 1, Vector3(0.5f, 0.4f, 0.25f),
                  Vector3(0.5f, 0.4f, 0.75f), -1.f },
                { "E5", 1, 1, Vector3(0.5f, 0.4f, 0.25f),
                  Vector3(0.5f, 0.4f, 0.7f), 0.25f },
                { "E6", 1, 1, Vector3(0.5f, 0.4f, 0.25f),
                  Vector3(0.5f, 0.4f, 0.3f), -1.f },
                { "E7", 0, 2, Vector3(0.15f, 0.4f, 0.4f),
                  Vector3(0.85f, 0.4f, 0.5f), -1.f },
                { "E8", 1, 1, Vector3(0.5f, 0.4f, 0.25f),
                  Vector3(0.5f, 0.4f, 0.75f), 0.25f },
                { "E9", 1, 1, Vector3(0.5f, 0.4f, 0.25f),
                  Vector3(0.5f, 0.4f, 0.75f), -1.f },
            };
            for (size_t m = 0; m < sizeof(steps) / sizeof(steps[0]); ++m)
            {
                if (m == 2)
                {
                    // between E2 and E3: the public invalidation face
                    eNavigator.clearCachedWayPath();
                    printf("ECLEAR\n");
                }
                NavLoc src(pChunk[steps[m].srcChunk], steps[m].src, 0.6f);
                NavLoc dst(pChunk[steps[m].dstChunk], steps[m].dst, 0.6f);
                NavLoc next;
                bool  passedActivatedPortal = false;
                if (eNavigator.findPath(src, dst, steps[m].limit, false, next,
                        passedActivatedPortal))
                {
                    printf("EOK %s %.4f %.6f %.6f %.6f %d %d %d\n",
                           steps[m].name, steps[m].limit, next.point().x,
                           next.point().y, next.point().z,
                           int(next.waypoint()),
                           setIdxOf(next.pSet().get()),
                           passedActivatedPortal ? 1 : 0);
                }
                else
                {
                    printf("ENONE %s %.4f\n", steps[m].name, steps[m].limit);
                }
            }
        }

        // ---- E10: the same query over a rebuilt resource ----
        // The variant view rebuilds chunk B un-split (one waypoint over the
        // whole strip). asBinary runs in memory — the pinned
        // world_navmesh_cross.bin on disk stays world 1's — the decode
        // walks the same record layout, and the set mounts on its own
        // chunk like the others. eNavigator keeps E1-E9's cache, whose
        // destination still names world 1's (B, wp1).
        {
            CrossView varView;
            varView.addPoly(LO, HI, 0,
                {
                    { 0.f,       0.f, 0, true },
                    { 0.f,       1.f, 0, true },
                    { 1.f / 3.f, 1.f, 0, true },
                    { 1.f / 3.f, 0.f, 0, true },
                });
            varView.addPoly(LO, HI, 1,
                {
                    // poly B whole, CW — no z = 1/2 split
                    { 1.f / 3.f, 0.f, 0, true },
                    { 1.f / 3.f, 1.f, 0, true },
                    { 2.f / 3.f, 1.f, 0, true },
                    { 2.f / 3.f, 0.f, 0, true },
                });
            varView.addPoly(LO, HI, 2,
                {
                    { 2.f / 3.f, 0.f, 0, true },
                    { 2.f / 3.f, 1.f, 0, true },
                    { 1.f,       1.f, 0, true },
                    { 1.f,       0.f, 0, true },
                });
            BinaryPtr varBlob = varView.asBinary(Matrix::identity, 0.f);
            const char* vdata = varBlob->cdata();

            // decode set 1 only, skipping set 0's record the way the
            // navmeshFactory walk does
            const char* vptr = vdata;
            {
                int32_t swps  = *(const int32_t*)(vptr + 8);
                int32_t sedge = *(const int32_t*)(vptr + 12);
                vptr += 16;
                vptr += swps * 12;
                vptr += sedge * 12;
            }
            int32_t version  = *(const int32_t*)vptr; vptr += 4;
            float   girth    = *(const float*)vptr;   vptr += 4;
            int32_t numWps   = *(const int32_t*)vptr; vptr += 4;
            int32_t numEdges = *(const int32_t*)vptr; vptr += 4;
            printf("EHDR version %d girth %.6f waypoints %d edges %d\n",
                   version, girth, numWps, numEdges);

            ChunkWaypointSetDataPtr pVarData =
                new ChunkWaypointSetData(2 /* base: set 0 has one wp */);
            pVarData->girth(0.6f);
            vptr = pVarData->readWaypointSet(vptr, numWps, numEdges);
            const ChunkWaypoints& vwps = pVarData->waypoints();
            for (size_t i = 0; i < vwps.size(); ++i)
            {
                printf("EWP %d minh %.6f maxh %.6f edges %d centre %.6f %.6f\n",
                       int(i), vwps[i].minHeight_, vwps[i].maxHeight_,
                       int(vwps[i].edgeCount_), vwps[i].centre_.x,
                       vwps[i].centre_.y);
            }

            // the FNF warning goes to stderr unbuffered; flush stdout so it
            // lands on its own line instead of inside a buffered print
            fflush(stdout);
            Chunk* pChunkV =
                new Chunk("chunkBV", pMapping, Matrix::identity, localBB);
            pChunkV->load(NULL);
            ChunkWaypointSetPtr pWayV = new ChunkWaypointSet(pVarData);
            pWayV->chunk(pChunkV);
            ChunkNavigator::instance(*pChunkV).add(pWayV.get());
            // the freelist double-destruct pin, same as the world-1 sets
            for (int pin = 0; pin < 512; ++pin)
            {
                pWayV->incRef();
            }
            wayPtr[3] = pWayV.get();

            NavLoc vsrc(pChunkV, Vector3(0.5f, 0.4f, 0.25f), 0.6f);
            NavLoc vdst(pChunkV, Vector3(0.5f, 0.4f, 0.75f), 0.6f);
            NavLoc vnext;
            bool   vact = false;
            if (eNavigator.findPath(vsrc, vdst, 0.25f, false, vnext, vact))
            {
                printf("EOK E10 0.2500 %.6f %.6f %.6f %d %d %d\n",
                       vnext.point().x, vnext.point().y, vnext.point().z,
                       int(vnext.waypoint()), setIdxOf(vnext.pSet().get()),
                       vact ? 1 : 0);
            }
            else
            {
                printf("ENONE E10 0.2500\n");
            }
        }
    }

    printf("RESULT OK\n");

    return 0;
}
