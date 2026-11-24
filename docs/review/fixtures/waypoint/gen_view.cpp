// BW-10-05-04 reference harness: compiles the ORIGINAL
// lib/waypoint_generator/waypoint_view.cpp unmodified against shadow
// stubs (/tmp/viewstub: chunk + resmgr faces as in-memory stand-ins) and
// dumps IWaypointView::asBinary records and the saveOut merge result as
// text. The truth log is docs/review/fixtures/waypoint/gen_view.log; the
// Rust port (crates/waypoint-gen/src/view.rs) re-emits the same text and
// compares line by line.
//
// Test view: 5 polygons over 2 sets on a grid (min (10,0,20), res 0.5),
// covering every adjacency arm of asBinary:
//   P0 v0 adj 1  (1-based self, in-set remap -> indexMap[1] = 0)
//   P0 v1 adj 3  (1-based P3, other set -> std::map phantom insert -> 0)
//   P0 v2 adj 0 + adjToAnotherChunk -> 65535
//   P0 v3 adj -1 vista -> ~-(-1) = -2
//   P1 v0 adj 2  (1-based P2, in-set remap -> indexMap[2] = 1)
//   P1 v1 adj 7  (out of range: > polygonCount -> raw passthrough)
//   P1 v2 adj -5 vista -> ~5 = -6
//   P2 v0 adj 0 + adjToAnotherChunk -> 65535
//   P3 v0 adj 4  (1-based P4, in-set remap -> indexMap[4] = 0)
//   P3 v1 adj 1  (1-based P0, other set -> phantom 0)
//   P3 v2 adj -2 vista -> -3
//   P3 v3 adj 5  (1-based P4, same set remap -> indexMap[5] = 1)
//   P4 v0 adj -1 vista -> -2
//   P4 v1 adj 1  (1-based P0, other set -> phantom 0)
// getGirth(0) = 0 -> falls back to defaultGirth 0.8; getGirth(1) = 1.2.
//
// build:
//   cp $BW/lib/waypoint_generator/waypoint_view.{hpp,cpp} /tmp/viewstub/waypoint_generator/
//   g++ -std=c++14 -O2 -o gen_view gen_view.cpp \
//     /tmp/viewstub/waypoint_generator/waypoint_view.cpp \
//     -I /tmp/viewstub -I $BW/lib \
//     -DCODE_INLINE -DHANDLE=void* -DINVALID_HANDLE_VALUE=NULL -DBW_GUARD= \
//     $BW/build/el7/lib/libcstdmf.a $BW/build/el7/lib/libmath.a -lpthread
// (stderr carries INFO_MSG; only stdout is the log)

#include "pch.hpp"
#include "waypoint_view.hpp"

#include "resmgr/bin_section.hpp"
#include "resmgr/bwresource.hpp"
#include "resmgr/datasection.hpp"
#include "waypoint/waypoint.hpp"

#include <cstdio>
#include <cstring>
#include <vector>

BW_BEGIN_NAMESPACE

namespace
{

unsigned long long fnv1a64( const char * data, size_t len )
{
	unsigned long long hash = 14695981039346656037ULL;
	for (size_t i = 0; i < len; ++i)
	{
		hash ^= (unsigned char)data[i];
		hash *= 1099511628211ULL;
	}
	return hash;
}

// One authored vertex: grid coords + raw adjacency + chunk-border flag.
struct TestVertex
{
	float gx, gz;
	int adj;
	bool chunk;
};

struct TestPoly
{
	int set;
	float minh, maxh;
	std::vector< TestVertex > verts;
};

class TestView : public IWaypointView
{
public:
	// grid faces (VertexRef scales by resolution and offsets by gridMin)
	uint32 gridX() const override { return 9; }
	uint32 gridZ() const override { return 9; }
	uint8 gridMask( int, int ) const override { return 0; }
	Vector3 gridMin() const override { return Vector3( 10.f, 0.f, 20.f ); }
	Vector3 gridMax() const override { return Vector3( 14.5f, 0.f, 24.5f ); }
	float gridResolution() const override { return 0.5f; }

	int getPolygonCount() const override { return int( polys_.size() ); }
	float getGirth( int set ) const override
		{ return set == 0 ? 0.f : 1.2f; }
	int getVertexCount( int polygon ) const override
		{ return int( polys_[ polygon ].verts.size() ); }
	float getMinHeight( int polygon ) const override
		{ return polys_[ polygon ].minh; }
	float getMaxHeight( int polygon ) const override
		{ return polys_[ polygon ].maxh; }
	int getSet( int polygon ) const override { return polys_[ polygon ].set; }
	int getSets() const override { return 2; }
	void getVertex( int polygon, int vertex, Vector2 & v,
		int & adjacency, bool & adjToAnotherChunk ) const override
	{
		const TestVertex & tv = polys_[ polygon ].verts[ vertex ];
		v = Vector2( tv.gx, tv.gz );
		adjacency = tv.adj;
		adjToAnotherChunk = tv.chunk;
	}
	BW::vector<int> findWaypoints( const Vector3 &, float ) const override
		{ return BW::vector<int>(); }
	const BW::string & identifier() const override { return id_; }

	std::vector< TestPoly > polys_;
	BW::string id_ = "gen_view";
};

void dumpBinary( const char * what, BinaryPtr bin )
{
	printf( "%s bytes %zu fnv %016llx\n", what, bin->len(),
		fnv1a64( bin->cdata(), bin->len() ) );
	// byte-exact cross-check: 16 bytes per HX line
	for (size_t i = 0; i < bin->len(); i += 16)
	{
		printf( "HX %04zx ", i );
		char chunk[33];
		for (size_t j = 0; j < 16 && i + j < bin->len(); ++j)
			sprintf( chunk + j * 2, "%02x",
				(unsigned char)bin->cdata()[ i + j ] );
		chunk[ 32 ] = 0;
		printf( "%s\n", chunk );
	}

	const char * ptr = bin->cdata();
	const char * end = ptr + bin->len();
	int record = 0;
	while (ptr < end)
	{
		int version = *((int*&)ptr)++;
		float girth = *((float*&)ptr)++;
		int navPolyElts = *((int*&)ptr)++;
		int navPolyEdges = *((int*&)ptr)++;
		printf( "R %d version %d girth %.1f nwp %d ne %d\n",
			record, version, girth, navPolyElts, navPolyEdges );
		// the record packs [header][all ctrl][all data] — decode in that
		// order, not interleaved per polygon
		const char * ctrl = ptr;
		ptr += navPolyElts * 12;
		for (int p = 0; p < navPolyElts; ++p)
		{
			const char * c = ctrl + p * 12;
			float minh = *((float*&)c)++;
			float maxh = *((float*&)c)++;
			int vc = *((int*&)c)++;
			printf( "  P %d minh %.1f maxh %.1f vc %d\n", p, minh, maxh, vc );
		}
		for (int v = 0; v < navPolyEdges; ++v)
		{
			float x = *((float*&)ptr)++;
			float z = *((float*&)ptr)++;
			int adj = *((int*&)ptr)++;
			printf( "  V %d x %.1f z %.1f adj %d\n", v, x, z, adj );
		}
		++record;
	}
}

// three authored old records for the saveOut merge: the 0.8 record hits
// the default girth (dropped), 1.5 and 0.6 are kept
BinaryPtr makeOldNavmesh()
{
	std::vector< char > old;
	auto pushInt = [&]( int v ) {
		old.insert( old.end(),
			reinterpret_cast< const char * >( &v ),
			reinterpret_cast< const char * >( &v ) + 4 );
	};
	auto pushFloat = [&]( float v ) {
		old.insert( old.end(),
			reinterpret_cast< const char * >( &v ),
			reinterpret_cast< const char * >( &v ) + 4 );
	};
	// record A: girth 0.8 = defaultGirth (dropped on merge)
	pushInt( 0 ); pushFloat( 0.8f ); pushInt( 1 ); pushInt( 1 );
	pushFloat( 9.f ); pushFloat( 9.f ); pushInt( 1 );	// poly: 1 vertex
	pushFloat( 11.f ); pushFloat( 21.f ); pushInt( 0 );	// vertex
	// record B: girth 1.5, empty (kept, 16 bytes)
	pushInt( 0 ); pushFloat( 1.5f ); pushInt( 0 ); pushInt( 0 );
	// record C: girth 0.6, one edge only (kept)
	pushInt( 0 ); pushFloat( 0.6f ); pushInt( 0 ); pushInt( 1 );
	pushFloat( 12.f ); pushFloat( 22.f ); pushInt( -4 );
	return BinaryPtr( new BinaryBlock(
		old.empty() ? NULL : &old[0], old.size(), "BinaryBlock/old" ) );
}

} // anonymous namespace

BW_END_NAMESPACE

int main()
{
	using namespace BW;

	TestView view;
	view.polys_.resize( 5 );
	view.polys_[0].set = 0; view.polys_[0].minh = 0.5f; view.polys_[0].maxh = 0.5f;
	view.polys_[0].verts.push_back( TestVertex{ 1.f, 1.f, 1, false } );
	view.polys_[0].verts.push_back( TestVertex{ 2.f, 1.f, 3, false } );
	view.polys_[0].verts.push_back( TestVertex{ 2.f, 2.f, 0, true } );
	view.polys_[0].verts.push_back( TestVertex{ 1.f, 2.f, -1, false } );
	view.polys_[1].set = 0; view.polys_[1].minh = 1.5f; view.polys_[1].maxh = 1.5f;
	view.polys_[1].verts.push_back( TestVertex{ 2.f, 1.f, 2, false } );
	view.polys_[1].verts.push_back( TestVertex{ 3.f, 1.f, 7, false } );
	view.polys_[1].verts.push_back( TestVertex{ 3.f, 2.f, -5, false } );
	view.polys_[2].set = 0; view.polys_[2].minh = 2.5f; view.polys_[2].maxh = 2.5f;
	view.polys_[2].verts.push_back( TestVertex{ 3.f, 2.f, 0, true } );
	view.polys_[3].set = 1; view.polys_[3].minh = 0.5f; view.polys_[3].maxh = 0.5f;
	view.polys_[3].verts.push_back( TestVertex{ 1.f, 4.f, 4, false } );
	view.polys_[3].verts.push_back( TestVertex{ 2.f, 4.f, 1, false } );
	view.polys_[3].verts.push_back( TestVertex{ 2.f, 5.f, -2, false } );
	view.polys_[3].verts.push_back( TestVertex{ 1.f, 5.f, 5, false } );
	view.polys_[4].set = 1; view.polys_[4].minh = 1.f; view.polys_[4].maxh = 1.f;
	view.polys_[4].verts.push_back( TestVertex{ 1.f, 5.f, -1, false } );
	view.polys_[4].verts.push_back( TestVertex{ 2.f, 5.f, 1, false } );

	Chunk chunk;
	chunk.transformInverse_ = Matrix::identity;
	chunk.centre_ = Vector3( 4.f, 0.f, 4.f );
	chunk.binFileName_ = "gen_view.cdata";
	chunk.resourceID_ = "gen_view.chunk";
	chunk.identifier_ = "gen_view";
	chunk.ownsMinX_ = -100.f; chunk.ownsMaxX_ = 3.5f;
	chunk.ownsMinZ_ = -100.f; chunk.ownsMaxZ_ = 100.f;
	chunk.space_.minX_ = -100.f; chunk.space_.maxX_ = 100.f;
	chunk.space_.minZ_ = -100.f; chunk.space_.maxZ_ = 100.f;

	printf( "VIEW polys %d\n", view.getPolygonCount() );

	// --- asBinary: the record stream itself
	dumpBinary( "ASBINARY", view.asBinary( Matrix::identity, 0.8f ) );

	// --- saveOut pass 1: no cdata yet -> creates one from scratch
	// (the .chunk section must exist or saveOut's legacy-section sweep
	// dereferences NULL; a legacy boundary child must be swept too)
	DataSectionPtr chunkSect( new DataSection( "gen_view.chunk" ) );
	chunkSect->newSection( "boundary" );
	chunkSect->newSection( "navPolySet" );
	BWResource::instance().store( "gen_view.chunk", chunkSect );
	view.saveOut( &chunk, 0.8f, true );

	DataSectionPtr cdata = BWResource::openSection( "gen_view.cdata" );
	printf( "SAVEOUT 1 cdata %d children\n", cdata->countChildren() );
	dumpBinary( "CDATA1",
		cdata->readBinary( "worldNavmesh" ) );
	printf( "CHUNK sects %d resource %s boundary %d navpolyset %d\n",
		chunkSect->countChildren(),
		chunkSect->strings_.count( "worldNavmesh/resource" )
			? chunkSect->strings_[ "worldNavmesh/resource" ].c_str() : "(none)",
		chunkSect->openSection( "boundary" ) != NULL,
		chunkSect->openSection( "navPolySet" ) != NULL );

	// --- saveOut pass 2: old navmesh in place, removeAllOld false ->
	// default-girth records are dropped, other girths kept in order
	cdata->writeBinary( "worldNavmesh", makeOldNavmesh() );
	view.saveOut( &chunk, 0.8f, false );
	dumpBinary( "CDATA2",
		BWResource::openSection( "gen_view.cdata" )->readBinary( "worldNavmesh" ) );

	return 0;
}
