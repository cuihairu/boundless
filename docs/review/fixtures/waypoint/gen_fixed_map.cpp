// BW-10-07-01 fixture generator: the fixed test map — one slope, one hole,
// one raised obstacle, and two adjacent chunks — with the truth probe
// printout the port replays.
//
// Compile like gen_waypoints_cross.cpp (links the reference el7 build of
// libwaypoint/libchunk/libresmgr/libmath/libcstdmf; the cmake-entitydef
// flags.make flag/define groups).
//
// The map: two inside chunks in the unit box (Chunk::load with a NULL
// section marks them loaded and resets the bounding box to [0,1]^3), a
// permissive portal pair on x = 1/2, girth 0.6 everywhere, polygons CLOCKWISE
// in the x-z plane like the engine's own navpolys (edge directions: west
// +z, north +x, east -z, south -x; vertex i's adjacency is the neighbour
// across the edge that starts at vertex i).
//
//   chunk A (set 0)  x in [0, 1/2]
//     A0w  global 1  x in [0.0, 0.2]  z in [0.0, 0.5]  band 0.2..0.6
//     A0e  global 2  x in [0.3, 0.5]  z in [0.0, 0.5]  band 0.2..0.6
//          the strip x in [0.2, 0.3] is the HOLE: both facing edges vista
//     A1   global 3  x in [0.0, 0.5]  z in [0.5, 1.0]  band 0.6..1.0  the SLOPE
//          its south edge is split adj-A0e / vista-hole / adj-A0w; the
//          touching bands (0.6 meets 0.6) keep the three linked
//   chunk B (set 1)  x in [1/2, 1.0]
//     B0   global 4  x in [0.5, 1.0]  z in [0.0, 0.4]  band 0.2..0.6
//          north edge split adj-B2w / vista-obstacle / adj-B2e
//     B1   global 5  x in [0.5, 1.0]  z in [0.6, 1.0]  band 0.2..0.6
//          south edge split adj-B2e / vista-obstacle / adj-B2w
//     B2w  global 6  x in [0.5, 0.65] z in [0.4, 0.6]  band 0.2..0.6
//     B2e  global 7  x in [0.85, 1.0] z in [0.4, 0.6]  band 0.2..0.6
//     B3   global 8  x in [0.65, 0.85] z in [0.4, 0.6] band 1.1..1.5 the OBSTACLE
//          the 0.1 band gap (0.6 vs 1.1) already breaks height adjacency;
//          every edge is also vista, so the top is graph-isolated
//
// Truth lines:
//   WROTE / HDR / SET / WP / EDGE     the blob decode (like 04-01/04-05)
//   PORTAL / CONNTO / EDGELABEL       the original bind()'s wiring
//   ITER                              the (born-dead) neighbour walks
//   NAVLOC                            where the original NavLoc resolver
//                                     drops each probe point — the hole
//                                     centre and the obstacle ground are
//                                     find_closest ties, the obstacle top
//                                     resolves into B3 by its band
//   XPATHBEGIN/XPATH/XPATHNONE/XPATHEND   chunk-level A*
//   SETSEARCHBEGIN/SETHOP/SETSEARCHNONE/SETSEARCHEND  set-level A*
//   FPBEGIN/FPOK/FPNONE/FPEND         the original navigator's findPath
//   FPOP/FFP/FFPNONE                  the original navigator's findFullPath
//   RESULT OK

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
// The minimal IWaypointView: eight polygons over two sets (one per chunk),
// positions in grid units (gridResolution 1.0, gridMin 0).
// ---------------------------------------------------------------------------
class FixedMapView : public IWaypointView
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
        std::vector<Vtx>     verts;
    };

    FixedMapView() : gridRes_(1.0f), gridMin_(0.f, 0.f, 0.f) {}

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
    int getSets() const override { return 2; }

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

int main()
{
    // The empty resource instance: GeometryMapping's settings lookup goes
    // through BWResource and must not assert on the missing singleton.
    BWResource bwResource;
    BWResource::instance().init( BW::vector< BW::string >() );

    FixedMapView view;
    const float LO_MIN = 0.2f, LO_MAX = 0.6f;    // the ground band
    const float SLOPE_MIN = 0.6f, SLOPE_MAX = 1.0f;  // A1, touching LO_MAX
    const float OBST_MIN = 1.1f, OBST_MAX = 1.5f;   // B3, the 0.1 gap

    // ---- chunk A, set 0 ----
    // A0w (global 1): west of the hole. CW: +z, +x, -z, -x.
    view.addPoly(LO_MIN, LO_MAX, 0,
        {
            { 0.f,  0.f,    0, true },  // west  -> world edge
            { 0.f,  0.5f,   3, false }, // north -> A1 (bands touch at 0.6)
            { 0.2f, 0.5f,  -1, false }, // east  -> the hole
            { 0.2f, 0.f,    0, true },  // south -> world edge
        });
    // A0e (global 2): east of the hole.
    view.addPoly(LO_MIN, LO_MAX, 0,
        {
            { 0.3f, 0.f,   -1, false }, // west  -> the hole
            { 0.3f, 0.5f,   3, false }, // north -> A1
            { 0.5f, 0.5f,   0, true },  // east  -> portal A|B
            { 0.5f, 0.f,    0, true },  // south -> world edge
        });
    // A1 (global 3): the slope, its south edge split three ways.
    view.addPoly(SLOPE_MIN, SLOPE_MAX, 0,
        {
            { 0.f,  0.5f,  0, true },   // west  -> world edge
            { 0.f,  1.f,   0, true },   // north -> world edge
            { 0.5f, 1.f,   0, true },   // east  -> portal A|B face
            { 0.5f, 0.5f,  2, false },  // south -> A0e
            { 0.3f, 0.5f, -1, false },  // south -> the hole
            { 0.2f, 0.5f,  1, false },  // south -> A0w
        });

    // ---- chunk B, set 1: the ring around the obstacle ----
    // B0 (global 4): the south strip, north edge split three ways.
    view.addPoly(LO_MIN, LO_MAX, 1,
        {
            { 0.5f, 0.f,    0, true },  // west  -> portal A|B
            { 0.5f, 0.4f,   6, false }, // north -> B2w
            { 0.65f, 0.4f, -1, false }, // north -> the obstacle face
            { 0.85f, 0.4f,  7, false }, // north -> B2e
            { 1.f,  0.4f,   0, true },  // east  -> world edge
            { 1.f,  0.f,    0, true },  // south -> world edge
        });
    // B1 (global 5): the north strip, south edge split three ways.
    view.addPoly(LO_MIN, LO_MAX, 1,
        {
            { 0.5f, 0.6f,   0, true },  // west  -> portal A|B
            { 0.5f, 1.f,    0, true },  // north -> world edge
            { 1.f,  1.f,    0, true },  // east  -> world edge
            { 1.f,  0.6f,   7, false }, // south -> B2e
            { 0.85f, 0.6f, -1, false }, // south -> the obstacle face
            { 0.65f, 0.6f,  6, false }, // south -> B2w
        });
    // B2w (global 6): the west rung.
    view.addPoly(LO_MIN, LO_MAX, 1,
        {
            { 0.5f,  0.4f, 0, true },   // west  -> portal A|B
            { 0.5f,  0.6f, 5, false },  // north -> B1
            { 0.65f, 0.6f, -1, false }, // east  -> the obstacle
            { 0.65f, 0.4f, 4, false },  // south -> B0
        });
    // B2e (global 7): the east rung.
    view.addPoly(LO_MIN, LO_MAX, 1,
        {
            { 0.85f, 0.4f, -1, false }, // west  -> the obstacle
            { 0.85f, 0.6f,  5, false }, // north -> B1
            { 1.f,   0.6f,  0, true },  // east  -> world edge
            { 1.f,   0.4f,  4, false }, // south -> B0
        });
    // B3 (global 8): the obstacle top — the 0.1 band gap plus all-vista
    // edges leave it graph-isolated.
    view.addPoly(OBST_MIN, OBST_MAX, 1,
        {
            { 0.65f, 0.4f, -1, false }, // west
            { 0.65f, 0.6f, -1, false }, // north
            { 0.85f, 0.6f, -1, false }, // east
            { 0.85f, 0.4f, -1, false }, // south
        });

    // ---- original writer: version 0 multi-set record ----
    BinaryPtr blob = view.asBinary(Matrix::identity, 0.f);
    const char*  data = blob->cdata();
    const size_t len  = blob->len();

    FILE* out = fopen("./world_navmesh_fixed_map.bin", "wb");
    fwrite(data, 1, len, out);
    fclose(out);
    printf("WROTE ./world_navmesh_fixed_map.bin %zu bytes\n", len);

    // ---- original reader: decode truth, two sets ----
    ChunkWaypointSetData* pSetData[2];
    int base = 1;
    for (int s = 0; s < 2; ++s)
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

    // ---- two inside chunks, marked loaded the way the loader does ----
    {
        ChunkSpacePtr pSpace = new ChunkSpace(7);
        XMLSectionPtr pSettings = new XMLSection("space");
        GeometryMapping* pMapping =
            new GeometryMapping(pSpace, Matrix::identity, ".", pSettings);
        BoundingBox localBB(Vector3(-10.f, -10.f, -10.f),
                            Vector3(50.f, 50.f, 50.f));

        Chunk* pChunk[2];
        const char* names[2] = { "chunkA", "chunkB" };
        for (int c = 0; c < 2; ++c)
        {
            pChunk[c] = new Chunk(names[c], pMapping, Matrix::identity, localBB);
            // A NULL section takes the loader's FNF branch: the chunk comes
            // out loaded() with its bounding box reset to [0,1]^3.
            pChunk[c]->load(NULL);
        }

        // ---- mount the sets on their chunks' navigators ----
        ChunkWaypointSetPtr pWay[2];
        for (int s = 0; s < 2; ++s)
        {
            pWay[s] = new ChunkWaypointSet(ChunkWaypointSetDataPtr(pSetData[s]));
            pWay[s]->chunk(pChunk[s]);
            ChunkNavigator::instance(*pChunk[s]).add(pWay[s].get());
            // The reference free list destructs each recycled search state
            // twice (eager ~IntState in free(), then delete in the list
            // destructor), so every A* node drops one set reference too
            // many. Same pin as gen_waypoints_cross.cpp; the algorithm the
            // probes exercise stays untouched.
            for (int pin = 0; pin < 512; ++pin)
            {
                pWay[s]->incRef();
            }
        }

        // ---- the portal pair ----
        // A|B on x = 1/2, PERMISSIVE in both directions (the Bot-crossing
        // acceptance walks through it). The 2D points are the (x, z)
        // rectangle corners in clockwise order, which is what
        // findBetterPortal's inside test needs (its cross products must all
        // come out positive). The rect stops short of the full edge
        // (z in [0.1, 0.9]) so the binding proves the partial-edge
        // intersection too.
        PlaneEq planeAB(Vector3(1.f, 0.f, 0.f), 0.5f);
        BW::vector<Vector3> rectAB;
        rectAB.push_back(Vector3(0.47f, 0.90f, 0.f));
        rectAB.push_back(Vector3(0.47f, 0.10f, 0.f));
        rectAB.push_back(Vector3(0.53f, 0.10f, 0.f));
        rectAB.push_back(Vector3(0.53f, 0.90f, 0.f));

        ChunkBoundary::Portal* portals[2];
        // 0: chunk A east -> B (permissive)
        portals[0] = new ChunkBoundary::Portal(planeAB, Vector3(0, 1, 0),
            rectAB, pMapping, "");
        portals[0]->pChunk = pChunk[1];
        // 1: chunk B west -> A (permissive)
        portals[1] = new ChunkBoundary::Portal(planeAB, Vector3(0, 1, 0),
            rectAB, pMapping, "");
        portals[1]->pChunk = pChunk[0];

        // bind()'s projection reconstructs the far point as
        // origin + uAxis*(uAxis . lwv) + vAxis*(vAxis . lwv); the identity
        // chunks want x and z preserved and the height landed mid-band.
        for (int i = 0; i < 2; ++i)
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

        ChunkBoundary* pBoundB = new ChunkBoundary(planeAB, portals[1]);
        pBoundB->bindPortal(0);
        pChunk[1]->joints().push_back(ChunkBoundaryPtr(pBoundB));

        printf("PORTALS 2\n");
        for (int i = 0; i < 2; ++i)
        {
            printf("PORTAL %d %d %d\n", i, portals[i]->permissive ? 1 : 0,
                   portals[i]->hasChunkItem() ? 1 : 0);
        }

        // ---- the original bind: real connections on both sides ----
        for (int s = 0; s < 2; ++s)
        {
            pWay[s]->bind();
        }

        // set identity helper: pointer -> index
        ChunkWaypointSet* wayPtr[2];
        for (int s = 0; s < 2; ++s) wayPtr[s] = pWay[s].get();
        auto setIdxOf = [&](ChunkWaypointSet* p) -> int {
            for (int s = 0; s < 2; ++s)
                if (p == wayPtr[s]) return s;
            return -1;
        };
        auto portalIdxOf = [&](ChunkBoundary::Portal* p) -> int {
            for (int i = 0; i < 2; ++i)
                if (p == portals[i]) return i;
            return -1;
        };

        // ---- the created connections and edge labels, straight off the
        // reference containers ----
        for (int s = 0; s < 2; ++s)
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
        for (int s = 0; s < 2; ++s)
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

        // ---- where the original NavLoc resolver drops each probe point ----
        {
            struct LocProbe
            {
                const char* name;
                int         chunk;
                Vector3     point;
            };
            const LocProbe probes[] = {
                // the plain ground points
                { "NA0W", 0, Vector3(0.1f, 0.4f, 0.25f) },
                { "NSLOPE", 0, Vector3(0.25f, 0.8f, 0.75f) },
                // the hole centre: no poly contains it, the find_closest
                // fallback ties A0w/A0e at 0.05 — the first-iterated wins
                { "NHOLE", 0, Vector3(0.25f, 0.4f, 0.25f) },
                { "NB0", 1, Vector3(0.75f, 0.4f, 0.2f) },
                // the obstacle top resolves by its own band
                { "NOBSTTOP", 1, Vector3(0.75f, 1.3f, 0.5f) },
                // the obstacle ground: B3's band misses, the fallback ties
                // B2w/B2e at 0.1 — the first-iterated wins
                { "NOBSTGROUND", 1, Vector3(0.75f, 0.4f, 0.5f) },
            };
            for (size_t p = 0; p < sizeof(probes) / sizeof(probes[0]); ++p)
            {
                NavLoc loc(pChunk[probes[p].chunk], probes[p].point, 0.6f);
                if (loc.pSet())
                {
                    printf("NAVLOC %s %d %d %.6f %.6f %.6f\n", probes[p].name,
                           int(loc.waypoint()), setIdxOf(loc.pSet().get()),
                           loc.point().x, loc.point().y, loc.point().z);
                }
                else
                {
                    printf("NAVLOCNONE %s\n", probes[p].name);
                }
            }
        }

        // ---- chunk-level A* probes ----
        {
            struct PathProbe
            {
                const char* name;
                Vector3     src;
                Vector3     dst;
            };
            const PathProbe probes[] = {
                // across the permissive portal, both ways
                { "CROSS",   Vector3(0.1f, 0.4f, 0.25f), Vector3(0.75f, 0.4f, 0.2f) },
                { "CROSSBA", Vector3(0.75f, 0.4f, 0.2f), Vector3(0.1f, 0.4f, 0.25f) },
                // the hole detour: A0w -> A1 (the slope) -> A0e
                { "HOLE",    Vector3(0.1f, 0.4f, 0.25f), Vector3(0.4f, 0.4f, 0.25f) },
                // around the obstacle through the ring
                { "OBST",    Vector3(0.575f, 0.4f, 0.5f), Vector3(0.925f, 0.4f, 0.5f) },
                // the obstacle top is graph-isolated
                { "WALLTOP", Vector3(0.575f, 0.4f, 0.5f), Vector3(0.75f, 1.3f, 0.5f) },
            };
            for (size_t p = 0; p < sizeof(probes) / sizeof(probes[0]); ++p)
            {
                int srcChunk = probes[p].src.x < 0.5f ? 0 : 1;
                int dstChunk = probes[p].dst.x < 0.5f ? 0 : 1;

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
                // the permissive pair answers regardless of the block flag
                { "AB0", Vector3(0.1f, 0.4f, 0.25f), Vector3(0.75f, 0.4f, 0.2f), 0 },
                { "AB1", Vector3(0.1f, 0.4f, 0.25f), Vector3(0.75f, 0.4f, 0.2f), 1 },
                { "BA0", Vector3(0.75f, 0.4f, 0.2f), Vector3(0.1f, 0.4f, 0.25f), 0 },
            };
            for (size_t p = 0; p < sizeof(probes) / sizeof(probes[0]); ++p)
            {
                int srcChunk = probes[p].src.x < 0.5f ? 0 : 1;
                int dstChunk = probes[p].dst.x < 0.5f ? 0 : 1;

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

        // ---- the original navigator's findPath ----
        {
            struct FindProbe
            {
                const char* name;
                Vector3     src;
                Vector3     dst;
            };
            const FindProbe probes[] = {
                // CROSS then its exact repeat exercises the cache-hit arm
                // (findWayPath + moveStateAlongPath).
                { "CROSS",   Vector3(0.1f, 0.4f, 0.25f), Vector3(0.75f, 0.4f, 0.2f) },
                { "CROSSR",  Vector3(0.1f, 0.4f, 0.25f), Vector3(0.75f, 0.4f, 0.2f) },
                { "HOLE",    Vector3(0.1f, 0.4f, 0.25f), Vector3(0.4f, 0.4f, 0.25f) },
                { "HOLER",   Vector3(0.4f, 0.4f, 0.25f), Vector3(0.1f, 0.4f, 0.25f) },
                { "OBST",    Vector3(0.575f, 0.4f, 0.5f), Vector3(0.925f, 0.4f, 0.5f) },
                { "WALLTOP", Vector3(0.575f, 0.4f, 0.5f), Vector3(0.75f, 1.3f, 0.5f) },
            };
            Navigator navigator;
            for (size_t p = 0; p < sizeof(probes) / sizeof(probes[0]); ++p)
            {
                int srcChunk = probes[p].src.x < 0.5f ? 0 : 1;
                int dstChunk = probes[p].dst.x < 0.5f ? 0 : 1;

                NavLoc src(pChunk[srcChunk], probes[p].src, 0.6f);
                NavLoc dst(pChunk[dstChunk], probes[p].dst, 0.6f);
                printf("FPBEGIN %s %d %d\n", probes[p].name,
                       int(src.waypoint()), int(dst.waypoint()));

                NavLoc next;
                bool passedActivatedPortal = false;
                if (navigator.findPath(src, dst, 1000000.f, false, next,
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

            // ---- the original navigator's findFullPath ----
            // The whole route's points, hop by hop — what the Bot crossing
            // and the navigation debug display consume. It re-uses the
            // findPath probes: the same endpoints, resolved through the
            // space's own girth lookup.
            Navigator full;
            for (size_t p = 0; p < sizeof(probes) / sizeof(probes[0]); ++p)
            {
                Vector3Path points;
                if (full.findFullPath(pSpace.get(), probes[p].src, probes[p].dst,
                        1000000.f, false, 0.6f, points))
                {
                    printf("FPOP %s %d\n", probes[p].name, int(points.size()));
                    for (size_t i = 0; i < points.size(); ++i)
                    {
                        printf("FFP %s %d %.6f %.6f %.6f\n", probes[p].name,
                               int(i), points[i].x, points[i].y, points[i].z);
                    }
                }
                else
                {
                    printf("FFPNONE %s\n", probes[p].name);
                }
            }
        }
    }

    printf("RESULT OK\n");

    return 0;
}
