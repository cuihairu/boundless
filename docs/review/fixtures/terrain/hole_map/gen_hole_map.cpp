// BW-10-03-02 fixture generator: original writer/reader code produces the
// sample hole map blobs and the truth probe printout.
//
// Links the reference el7 build of libcstdmf/libmath/libmoo/libresmgr/
// libterrain. The blobs are original-code output (TerrainHoleMap2::save =
// HolesHeader + packed bit rows, BinaryBlock-compressed into the section);
// the truth side loads them back through the original TerrainHoleMap2::load
// (with the section's compressed binary, i.e. the authentic engine path)
// and prints holeAt probes. The dumped .bin files are the decompressed
// payload, matching the port boundary (the resource layer hands over
// decompressed blocks).

#include <cmath>
#include <cstdio>
#include <cstring>
#include <string>
#include <vector>

#include "cstdmf/bgtask_manager.hpp"

#include "resmgr/bin_section.hpp"
#include "resmgr/binary_block.hpp"

#include "terrain/terrain2/common_terrain_block2.hpp"
#include "terrain/terrain2/terrain_hole_map2.hpp"
#include "terrain/terrain_settings.hpp"

using namespace BW;
using namespace BW::Terrain;

static const float BLOCK_SIZE = 100.0f;

// 10 % 8 = 2 keeps the reader stride quirk on the clean path: the
// load-side stride (width/8 + (width&3 ? 1 : 0)) equals the writer's
// ceil(width/8) for every width except width % 8 == 4.
static const uint32 SIZE = 10;

// Pattern cells: (3,2), (4,2), (3,3), (7,5), (0,0), (9,9), (5,7).
static const uint32 HOLES[][2] = {
    {3, 2}, {4, 2}, {3, 3}, {7, 5}, {0, 0}, {9, 9}, {5, 7},
};
static const uint32 NUM_HOLES = sizeof(HOLES) / sizeof(HOLES[0]);

static TerrainSettingsPtr makeSettings()
{
    TerrainSettingsPtr settings = new TerrainSettings();
    settings->init(BLOCK_SIZE, NULL);
    return settings;
}

// Save through the original face, then dump the section's binary after the
// same decompress step the loader performs.
static bool saveAndDump(Terrain::TerrainHoleMap2 &hm, const char *path)
{
    BinaryPtr dummy = new BW::BinaryBlock("d", 1, "Terrain/HoleMap2/dummy");
    BW::DataSectionPtr section = new BW::BinSection("holes", dummy);
    if (!hm.save(section))
    {
        printf("SAVEFAIL %s\n", path);
        return false;
    }
    BinaryPtr compressed = section->asBinary();
    if (!compressed || !compressed->isCompressed())
    {
        printf("NOTCOMPRESSED %s\n", path);
        return false;
    }
    BinaryPtr raw = compressed->decompress();

    FILE *f = ::fopen(path, "wb");
    ::fwrite(raw->data(), 1, raw->len(), f);
    ::fclose(f);
    printf("WROTE %s %u bytes\n", path, unsigned(raw->len()));
    return true;
}

// Load the compressed section back through the original reader (the
// authentic engine path: section binary -> isCompressed -> decompress)
// and print probe lines.
static bool loadAndProbe(const char *label, BinaryPtr compressed,
    const BW::vector<uint8> &raw)
{
    BW::DataSectionPtr section = new BW::BinSection("holes", compressed);
    CommonTerrainBlock2 block(makeSettings());
    Terrain::TerrainHoleMap2 hm(block);
    BW::string error;
    if (!hm.load(section, &error))
    {
        printf("LOADFAIL %s %s\n", label, error.c_str());
        return false;
    }

    printf("MAP %s width %u height %u holesSize %u holesMapSize %u "
           "noHoles %d allHoles %d\n",
        label, hm.width(), hm.height(), hm.holesSize(), hm.holesMapSize(),
        int(hm.noHoles()), int(hm.allHoles()));

    // Point probes at the cell centres plus the block edges.
    for (float z = 5.0f; z < BLOCK_SIZE; z += 10.0f)
    {
        for (float x = 5.0f; x < BLOCK_SIZE; x += 10.0f)
        {
            printf("HOLE %s %.2f %.2f %d\n", label, x, z,
                int(hm.holeAt(x, z)));
        }
    }
    // Edge and out-of-range probes (truncation toward zero, clamping).
    const float EDGE[][2] = {
        {0.0f, 0.0f}, {99.99f, 99.99f}, {-5.0f, -5.0f}, {105.0f, 50.0f},
        {50.0f, -1.0f}, {101.0f, 101.0f},
    };
    const uint32 NUM_EDGES = sizeof(EDGE) / sizeof(EDGE[0]);
    for (uint32 i = 0; i < NUM_EDGES; ++i)
    {
        printf("HOLE %s %.2f %.2f %d\n", label, EDGE[i][0], EDGE[i][1],
            int(hm.holeAt(EDGE[i][0], EDGE[i][1])));
    }

    // Rect probes: over holes, beside holes, touching a hole edge.
    const float RECTS[][4] = {
        {30.0f, 20.0f, 49.0f, 39.0f},  // covers (3,2),(4,2),(3,3)
        {50.0f, 50.0f, 59.0f, 59.0f},  // cell (5,5): beside (5,7)
        {30.0f, 20.0f, 30.0f, 20.0f},  // single point in (3,2)
        {29.99f, 20.0f, 29.99f, 39.0f}, // just left of column 3
        {70.0f, 50.0f, 79.0f, 59.0f},  // covers (7,5)
        {-50.0f, -50.0f, 200.0f, 200.0f}, // everything
    };
    const uint32 NUM_RECTS = sizeof(RECTS) / sizeof(RECTS[0]);
    for (uint32 i = 0; i < NUM_RECTS; ++i)
    {
        printf("RECT %s %.2f %.2f %.2f %.2f %d\n", label, RECTS[i][0],
            RECTS[i][1], RECTS[i][2], RECTS[i][3],
            int(hm.holeAt(RECTS[i][0], RECTS[i][1], RECTS[i][2],
                RECTS[i][3])));
    }
    (void)raw;
    return true;
}

int main(int argc, char *argv[])
{
    const char *outDir = argc > 1 ? argv[1] : ".";

    // --- fixture 1: mixed holes through create + image edits + unlock.
    {
        CommonTerrainBlock2 block(makeSettings());
        Terrain::TerrainHoleMap2 hm(block);
        hm.create(SIZE);
        hm.lock(false);
        for (uint32 i = 0; i < NUM_HOLES; ++i)
        {
            hm.image().set(int(HOLES[i][0]), int(HOLES[i][1]), true);
        }
        hm.unlock(); // recalc: mixed -> image kept
        char path[512];
        ::snprintf(path, sizeof(path), "%s/holes_mixed.bin", outDir);
        if (!saveAndDump(hm, path))
        {
            return 1;
        }
        // Truth: the compressed binary goes back through a section.
        BinaryPtr dummy = new BW::BinaryBlock("d", 1, "Terrain/HoleMap2/dummy");
        BW::DataSectionPtr section = new BW::BinSection("holes", dummy);
        hm.save(section);
        BinaryPtr compressed = section->asBinary();
        BinaryPtr rawBlock = compressed->decompress();
        const uint8 *rawData = (const uint8 *)rawBlock->data();
        BW::vector<uint8> raw(rawData, rawData + rawBlock->len());
        if (!loadAndProbe("mixed", compressed, raw))
        {
            return 1;
        }
    }

    // --- fixture 2: no holes at all (degenerate).
    BinaryPtr mixedCompressed;
    {
        CommonTerrainBlock2 block(makeSettings());
        Terrain::TerrainHoleMap2 hm(block);
        hm.create(SIZE);
        char path[512];
        ::snprintf(path, sizeof(path), "%s/holes_none.bin", outDir);
        if (!saveAndDump(hm, path))
        {
            return 1;
        }
        BinaryPtr dummy = new BW::BinaryBlock("d", 1, "Terrain/HoleMap2/dummy");
        BW::DataSectionPtr section = new BW::BinSection("holes", dummy);
        hm.save(section);
        BinaryPtr compressed = section->asBinary();
        if (!loadAndProbe("none", compressed, BW::vector<uint8>()))
        {
            return 1;
        }
    }

    // --- fixture 3: all holes (degenerate; save refills the image with
    // allHoles_ through the lock face).
    {
        CommonTerrainBlock2 block(makeSettings());
        Terrain::TerrainHoleMap2 hm(block);
        hm.create(SIZE);
        hm.lock(false);
        for (uint32 z = 0; z < SIZE; ++z)
        {
            for (uint32 x = 0; x < SIZE; ++x)
            {
                hm.image().set(int(x), int(z), true);
            }
        }
        hm.unlock(); // recalc: all -> image cleared, allHoles_ set
        char path[512];
        ::snprintf(path, sizeof(path), "%s/holes_all.bin", outDir);
        if (!saveAndDump(hm, path))
        {
            return 1;
        }
        BinaryPtr dummy = new BW::BinaryBlock("d", 1, "Terrain/HoleMap2/dummy");
        BW::DataSectionPtr section = new BW::BinSection("holes", dummy);
        hm.save(section);
        BinaryPtr compressed = section->asBinary();
        if (!loadAndProbe("all", compressed, BW::vector<uint8>()))
        {
            return 1;
        }
    }

    printf("RESULT OK\n");
    return 0;
}
