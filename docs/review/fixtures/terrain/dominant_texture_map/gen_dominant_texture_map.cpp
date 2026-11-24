// BW-10-03-03 fixture generator: the original DominantTextureMap2 reader
// answers the truth probes, and the sample blob is produced with the real
// DominantTextureMapHeader struct, the save routine's strncpy name windows
// and the real BinaryBlock::compress — the el7 server library is built
// without EDITOR_ENABLED, so DominantTextureMap2::save itself is not
// linkable; the blob layout follows its source lines verbatim
// (dominant_texture_map2.cpp:47-104) and is validated by loading it back
// through the original load face.

#include <cstdio>
#include <cstring>
#include <string>
#include <vector>

#include "cstdmf/bgtask_manager.hpp"

#include "resmgr/bin_section.hpp"
#include "resmgr/binary_block.hpp"

#include "terrain/dominant_texture_map.hpp"
#include "terrain/terrain2/dominant_texture_map2.hpp"
#include "terrain/terrain_data.hpp"

using namespace BW;
using namespace BW::Terrain;

static const float BLOCK_SIZE = 100.0f;

// 6x4 cells, three texture names of different lengths (the name window is
// the longest, 18; shorter names are NUL-padded by the save-side strncpy).
static const uint32 W = 6, H = 4;
static const char *NAMES[] = {
    "ground/grass0.dds", "rock", "sand/big_grain.dds",
};
static const uint32 NUM_NAMES = 3;

// Cell byte = (groundType << 4) | materialIndex. The index stays below
// NUM_NAMES (texture() MF_ASSERTs); the ground type walks 0..4.
static uint8 cellByte(uint32 x, uint32 y)
{
    uint32 index = (x + 2 * y) % NUM_NAMES;
    uint32 ground = (x / 2 + y) % 5;
    return uint8((ground << 4) | index);
}

// The save layout, line by line from DominantTextureMap2::save
// (dominant_texture_map2.cpp:47-104).
static BinaryPtr buildSaveData(const char *path)
{
    uint32 textureNameSize = 0;
    for (uint32 i = 0; i < NUM_NAMES; ++i)
    {
        uint32 len = uint32(::strlen(NAMES[i]));
        if (len > textureNameSize)
            textureNameSize = len;
    }
    uint32 numTextures = NUM_NAMES;
    uint32 textureDataSize = textureNameSize * numTextures;

    BW::vector<uint8> hdrData(
        sizeof(DominantTextureMapHeader) + textureDataSize, 0);
    DominantTextureMapHeader *pHeader =
        reinterpret_cast<DominantTextureMapHeader *>(&hdrData.front());
    pHeader->magic_ = DominantTextureMapHeader::MAGIC;
    pHeader->version_ = DominantTextureMapHeader::VERSION_ZIP;
    pHeader->numTextures_ = numTextures;
    pHeader->textureNameSize_ = textureNameSize;
    char *p = (char *)(pHeader + 1);
    for (uint32 i = 0; i < numTextures; ++i)
    {
        ::strncpy(p, NAMES[i], textureNameSize);
        p += textureNameSize;
    }
    pHeader->width_ = W;
    pHeader->height_ = H;

    BW::vector<uint8> imgData(W * H);
    for (uint32 y = 0; y < H; ++y)
    {
        for (uint32 x = 0; x < W; ++x)
        {
            imgData[y * W + x] = cellByte(x, y);
        }
    }

    BinaryPtr pImageBlock = new BinaryBlock(
        &imgData.front(), imgData.size(), "Terrain/DominantTextureMap2/BinaryBlock");
    pImageBlock = pImageBlock->compress();

    BW::vector<uint8> data(hdrData.size() + pImageBlock->len());
    ::memcpy(&data.front(), &hdrData.front(), hdrData.size());
    ::memcpy(&data.front() + hdrData.size(), pImageBlock->data(), pImageBlock->len());

    BinaryPtr pSaveData = new BinaryBlock(
        &data.front(), data.size(), "Terrain/DominantTextureMap2/BinaryBlock");
    pSaveData->canZip(false); // don't recompress

    FILE *f = ::fopen(path, "wb");
    ::fwrite(pSaveData->data(), 1, pSaveData->len(), f);
    ::fclose(f);
    printf("WROTE %s %u bytes\n", path, unsigned(pSaveData->len()));
    return pSaveData;
}

int main(int argc, char *argv[])
{
    const char *outDir = argc > 1 ? argv[1] : ".";
    char path[512];
    ::snprintf(path, sizeof(path), "%s/dominant.bin", outDir);

    BinaryPtr saveData = buildSaveData(path);

    // The section must hand the exact bytes back (canZip(false) honoured).
    BW::DataSectionPtr section = new BW::BinSection("dominantTexture", saveData);
    BinaryPtr sectionBinary = section->asBinary();
    if (!sectionBinary || sectionBinary->isCompressed())
    {
        printf("SECTIONBINARYFAIL\n");
        return 1;
    }

    // Load through the original reader and probe.
    DominantTextureMap2 map(BLOCK_SIZE);
    BW::string error;
    if (!map.load(section, &error))
    {
        printf("LOADFAIL %s\n", error.c_str());
        return 1;
    }

    printf("MAP width %u height %u numTextures %u textureNameSize %u\n",
        map.width(), map.height(), NUM_NAMES, unsigned(::strlen(NAMES[2])));
    for (uint32 i = 0; i < NUM_NAMES; ++i)
    {
        printf("NAME %u %s\n", i, NAMES[i]);
    }

    // Probe coordinates: a 10x10 grid, the rounding-crossing hairs for
    // both axes ((k-0.5)*blockSize/(dim-1)), and far/negative edges.
    BW::vector<float> xs, zs;
    for (float v = 5.0f; v < BLOCK_SIZE; v += 10.0f)
    {
        xs.push_back(v);
        zs.push_back(v);
    }
    const float hair[] = {
        0.0f, 9.99f, 10.0f, 29.99f, 30.0f, 49.99f, 50.0f, 69.99f, 70.0f,
        89.99f, 90.0f, 99.99f, 100.0f, 16.66f, 16.67f, 83.33f, 83.34f,
        -5.0f, -50.0f, 105.0f, 200.0f, -0.01f,
    };
    const uint32 NUM_HAIR = sizeof(hair) / sizeof(hair[0]);
    for (uint32 i = 0; i < NUM_HAIR; ++i)
    {
        xs.push_back(hair[i]);
        zs.push_back(hair[i]);
    }
    for (uint32 zi = 0; zi < zs.size(); ++zi)
    {
        for (uint32 xi = 0; xi < xs.size(); ++xi)
        {
            printf("TEX %.2f %.2f %s\n", xs[xi], zs[zi],
                map.texture(xs[xi], zs[zi]).c_str());
            printf("GT %.2f %.2f %u\n", xs[xi], zs[zi],
                unsigned(map.groundType(xs[xi], zs[zi])));
        }
    }

    printf("RESULT OK\n");
    return 0;
}
