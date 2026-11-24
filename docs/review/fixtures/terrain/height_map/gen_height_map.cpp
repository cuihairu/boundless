// BW-10-03-01 fixture generator: original writer/reader code produces the
// sample height map blobs and the truth probe printout.
//
// Compile (see gen_height_map.log for the exact command): links the reference
// el7 build of libcstdmf/libmath/libmoo/libresmgr/libterrain + system libpng.
// The blobs are original-code output (compressHeightMap = quantised PNG via
// libpng); the truth side loads them back through the original
// TerrainHeightMap2::load + heightAt faces and prints probe lines.

#include <cmath>
#include <cstdio>
#include <cstring>
#include <string>
#include <vector>

#include "cstdmf/bgtask_manager.hpp"

#include "moo/image.hpp"

#include "resmgr/bin_section.hpp"
#include "resmgr/binary_block.hpp"

#include "terrain/height_map_compress.hpp"
#include "terrain/terrain2/terrain_height_map2.hpp"

using namespace BW;
using namespace BW::Terrain;

typedef BW::Moo::Image<float> FloatImage;

static const float BLOCK_SIZE = 100.0f;

// Deterministic heights quantised to the millimetre grid so the quantised
// PNG round-trip is exact.
static float sampleHeight(int x, int y)
{
    double h = 10.0 * sin(0.7 * x) + 5.0 * cos(0.5 * y) + 0.02 * (x * x - y * y);
    return float(std::floor(h * 1000.0 + 0.5) / 1000.0);
}

static void fillImage(FloatImage &img)
{
    for (uint32 y = 0; y < img.height(); ++y)
    {
        for (uint32 x = 0; x < img.width(); ++x)
        {
            img.set(x, y, sampleHeight(x, y));
        }
    }
}

// The header assembly mirrors TerrainHeightMap2::save (editor-only in the
// reference): zeroed struct + magic + COMPRESS_RAW + dims + real min/max +
// version.  Everything after the header is original-code output.
static BW::vector<uint8> assembleHeader(const HeightMapHeader &hmh,
    BinaryPtr payload)
{
    BW::vector<uint8> data(sizeof(hmh) + payload->len());
    ::memcpy(&data[0], &hmh, sizeof(hmh));
    ::memcpy(&data[sizeof(hmh)], payload->data(), payload->len());
    return data;
}

static bool loadAndProbe(const char *label, const BW::vector<uint8> &data,
    uint32 polesWidth, uint32 polesHeight)
{
    BinaryPtr blob = new BW::BinaryBlock(&data[0], data.size(),
        "Terrain/HeightMap2/fixture");
    BW::DataSectionPtr section = new BW::BinSection("heights", blob);

    BW::Terrain::TerrainHeightMap2 hm2(BLOCK_SIZE);
    BW::string error;
    if (!hm2.load(section, &error))
    {
        printf("LOADFAIL %s %s\n", label, error.c_str());
        return false;
    }

    printf("MAP %s poles %u x %u\n", label, hm2.polesWidth(), hm2.polesHeight());
    printf("MAP %s min %.6f max %.6f\n", label, hm2.minHeight(),
        hm2.maxHeight());
    printf("MAP %s spacing %.6f %.6f\n", label, hm2.spacingX(), hm2.spacingZ());
    printf("MAP %s blocks %u %u vertices %u %u visibleOffset %u %u\n", label,
        hm2.blocksWidth(), hm2.blocksHeight(), hm2.verticesWidth(),
        hm2.verticesHeight(), hm2.xVisibleOffset(), hm2.zVisibleOffset());

    // Grid probes: heightAt(int, int) in visible coordinates.
    const uint32 vw = hm2.verticesWidth();
    const uint32 vh = hm2.verticesHeight();
    for (uint32 z = 0; z < vh; z += 5)
    {
        for (uint32 x = 0; x < vw; x += 5)
        {
            printf("GRID %s %u %u %.6f\n", label, x, z, hm2.heightAt(int(x), int(z)));
        }
    }
    // Corners at the far edges.
    printf("GRID %s %u %u %.6f\n", label, vw - 1, vh - 1,
        hm2.heightAt(int(vw - 1), int(vh - 1)));

    // Interpolated probes: heightAt(float, float) in block-local metres.
    for (float z = 0.0f; z <= BLOCK_SIZE; z += 5.0f)
    {
        for (float x = 0.0f; x <= BLOCK_SIZE; x += 5.0f)
        {
            printf("INTERP %s %.3f %.3f %.6f\n", label, x, z,
                hm2.heightAt(x, z));
        }
    }
    (void)polesWidth;
    (void)polesHeight;
    return true;
}

int main(int argc, char *argv[])
{
    const char *outDir = argc > 1 ? argv[1] : ".";

    // 45 poles = 40 blocks * 2.5 m + 1 + 2*2 invisible border, like create(40).
    const uint32 POLES = 45;
    FloatImage img(POLES, POLES, "Terrain/HeightMap2/fixture");
    fillImage(img);

    float realMin = img.get(0, 0);
    float realMax = img.get(0, 0);
    for (uint32 y = 0; y < POLES; ++y)
    {
        for (uint32 x = 0; x < POLES; ++x)
        {
            realMin = std::min(realMin, img.get(x, y));
            realMax = std::max(realMax, img.get(x, y));
        }
    }

    // --- fixture 1: VERSION_ABS_QFLOAT via the original compressHeightMap.
    HeightMapHeader hmh;
    ::memset(&hmh, 0, sizeof(hmh));
    hmh.magic_ = HeightMapHeader::MAGIC;
    hmh.compression_ = Terrain::COMPRESS_RAW;
    hmh.width_ = POLES;
    hmh.height_ = POLES;
    hmh.minHeight_ = realMin;
    hmh.maxHeight_ = realMax;
    hmh.version_ = HeightMapHeader::VERSION_ABS_QFLOAT;

    BinaryPtr qpng = Terrain::compressHeightMap(img);
    BW::vector<uint8> qdata = assembleHeader(hmh, qpng);

    char path[512];
    ::snprintf(path, sizeof(path), "%s/heightmap_qfloat.bin", outDir);
    FILE *f = ::fopen(path, "wb");
    ::fwrite(&qdata[0], 1, qdata.size(), f);
    ::fclose(f);
    printf("WROTE %s %u bytes\n", path, unsigned(qdata.size()));

    // --- fixture 2: VERSION_ABS_FLOAT with raw floats.
    HeightMapHeader fh = hmh;
    fh.version_ = HeightMapHeader::VERSION_ABS_FLOAT;
    BW::vector<uint8> fdata(sizeof(fh) + POLES * POLES * sizeof(float));
    ::memcpy(&fdata[0], &fh, sizeof(fh));
    {
        float *dst = (float *)(&fdata[sizeof(fh)]);
        for (uint32 y = 0; y < POLES; ++y)
        {
            for (uint32 x = 0; x < POLES; ++x)
            {
                *dst++ = img.get(x, y);
            }
        }
    }
    ::snprintf(path, sizeof(path), "%s/heightmap_absfloat.bin", outDir);
    f = ::fopen(path, "wb");
    ::fwrite(&fdata[0], 1, fdata.size(), f);
    ::fclose(f);
    printf("WROTE %s %u bytes\n", path, unsigned(fdata.size()));

    // --- truth: load both back through the original reader and probe.
    bool ok = loadAndProbe("qfloat", qdata, POLES, POLES);
    ok = loadAndProbe("absfloat", fdata, POLES, POLES) && ok;

    printf("RESULT %s\n", ok ? "OK" : "FAIL");
    return ok ? 0 : 1;
}
