// gen_portal — BW-10-03-07 truth fixture generator.
//
// Drives the ORIGINAL el7 library code end to end through the portal
// collision face and prints every result as bit-exact hex:
//   * `ChunkBoundary::Portal` plane constructor (chunk_boundary.cpp:491-586)
//   * `Portal::setCollisionState` (chunk_boundary.cpp:895-909) - the open /
//     closed switch (both sides of the battery run the identical probes)
//   * `PortalChunkItem::toss` -> `ChunkModelObstacle` (portal_chunk_item.cpp)
//   * `PortalObstacle` constructor + buildTriangles + both collide overloads
//     (portal_chunk_item.cpp:44-175) - the class is file-private in the
//     reference, so it is redeclared here verbatim; every method that runs is
//     the original object code from libchunk.a
//   * `SweepParams` + `SweepShape` (physics2/sweep_shape.hpp, header only)
//   * `SweepHelpers::collideIntoObject` (physics2/sweep_helpers.hpp) - the
//     world->chunk coordinate transform, bb clip with bloat and the
//     sTravel/eTravel remap
//   * `CollisionState` / `CollisionCallback::doVisit` say-bit protocol
//     (physics2/collision_callback.hpp, header only)
//
// The harness chunk is a real `Chunk` (outside "1_1", grid 100) under a real
// `GeometryMapping` whose mapper carries scale 2 / rotY 90 / translate, so
// the chunk transform (and its inverse, used by collideIntoObject) is
// non-trivial. Probe geometry is authored in chunk-local coordinates and
// mapped to world with the chunk's own transform (original Matrix code) -
// no hand-written math on the truth path.

#include "cstdmf/guard.hpp"

#include "math/mathdef.hpp"
#include "math/matrix.hpp"
#include "math/boundbox.hpp"

#include "physics2/worldtri.hpp"
#include "physics2/collision_callback.hpp"
#include "physics2/sweep_helpers.hpp"
#include "physics2/sweep_shape.hpp"

#include "chunk/chunk.hpp"
#include "chunk/chunk_space.hpp"
#include "chunk/geometry_mapping.hpp"
#include "chunk/chunk_boundary.hpp"
#include "chunk/portal_chunk_item.hpp"
#include "chunk/chunk_obstacle.hpp"

#include "resmgr/xml_section.hpp"

#include <cstdio>
#include <unistd.h>
#include <vector>

using namespace std;
using namespace BW;

// Verbatim redeclaration of the obstacle class that is file-private in
// portal_chunk_item.cpp. Member order must match the original so that the
// original constructor's layout is honoured; bb_ is declared public here so
// this harness can hand it to collideIntoObject (the reference never hands
// it out either - ChunkSpace keeps the obstacles and passes their bb_).
BW_BEGIN_NAMESPACE

class PortalObstacle : public ChunkObstacle
{
public:
	PortalObstacle( PortalChunkItem * pChunkItem );

	virtual bool collide( const Vector3 & source, const Vector3 & extent,
		CollisionState & state ) const;
	virtual bool collide( const WorldTriangle & source, const Vector3 & extent,
		CollisionState & state ) const;

	BoundingBox		bb_;
	mutable BW::vector<WorldTriangle>	ltris_;
};

BW_END_NAMESPACE

// ---------------------------------------------------------------------------
// bit-exact printing helpers
// ---------------------------------------------------------------------------

static const char * bits( float f )
{
	static char bufs[64][16];
	static unsigned next = 0;
	char * buf = bufs[ next++ & 63 ];
	union { float f; unsigned int u; } x;
	x.f = f;
	snprintf( buf, sizeof( bufs[0] ), "%08x", x.u );
	return buf;
}

static void printVec( const char * tag, const Vector3 & v )
{
	printf( "%s %s %s %s\n", tag, bits( v.x ), bits( v.y ), bits( v.z ) );
}

// One line: the three axis columns and the origin (12 floats = the linear
// part and the translation; the reference Matrix always leaves row 3 as
// (0,0,0,1) for this face).
static void printMat( const char * tag, const Matrix & m )
{
	Vector3 ax0 = m.applyToUnitAxisVector( 0 );
	Vector3 ax1 = m.applyToUnitAxisVector( 1 );
	Vector3 ax2 = m.applyToUnitAxisVector( 2 );
	Vector3 org = m.applyToOrigin();
	printf( "%s ", tag );
	printf( "%s %s %s ", bits( ax0.x ), bits( ax0.y ), bits( ax0.z ) );
	printf( "%s %s %s ", bits( ax1.x ), bits( ax1.y ), bits( ax1.z ) );
	printf( "%s %s %s ", bits( ax2.x ), bits( ax2.y ), bits( ax2.z ) );
	printf( "%s %s %s\n", bits( org.x ), bits( org.y ), bits( org.z ) );
}

// ---------------------------------------------------------------------------
// truth callback - prints every visit with the obstacle transform matrices,
// the hit triangle's vertices and flags, and the reported distance
// ---------------------------------------------------------------------------

class TruthCallback : public CollisionCallback
{
public:
	TruthCallback( const char * tag ) : tag_( tag ), visits_( 0 ),
		say_( COLLIDE_STOP )
	{
	}

	virtual int doVisit( const CollisionObstacle & obstacle,
		const WorldTriangle & triangle, float dist )
	{
		++visits_;

		const Matrix & m = obstacle.transform();
		const Matrix & mi = obstacle.transformInverse();

		const Vector3 & a = triangle.v0();
		const Vector3 & b = triangle.v1();
		const Vector3 & c = triangle.v2();

		printf( "%s VST M ", tag_ );
		Vector3 ma0 = m.applyToUnitAxisVector( 0 );
		Vector3 ma1 = m.applyToUnitAxisVector( 1 );
		Vector3 ma2 = m.applyToUnitAxisVector( 2 );
		Vector3 mo = m.applyToOrigin();
		printf( "%s %s %s ", bits( ma0.x ), bits( ma0.y ), bits( ma0.z ) );
		printf( "%s %s %s ", bits( ma1.x ), bits( ma1.y ), bits( ma1.z ) );
		printf( "%s %s %s ", bits( ma2.x ), bits( ma2.y ), bits( ma2.z ) );
		printf( "%s %s %s MI ", bits( mo.x ), bits( mo.y ), bits( mo.z ) );
		Vector3 ia0 = mi.applyToUnitAxisVector( 0 );
		Vector3 ia1 = mi.applyToUnitAxisVector( 1 );
		Vector3 ia2 = mi.applyToUnitAxisVector( 2 );
		Vector3 io = mi.applyToOrigin();
		printf( "%s %s %s ", bits( ia0.x ), bits( ia0.y ), bits( ia0.z ) );
		printf( "%s %s %s ", bits( ia1.x ), bits( ia1.y ), bits( ia1.z ) );
		printf( "%s %s %s ", bits( ia2.x ), bits( ia2.y ), bits( ia2.z ) );
		printf( "%s %s %s WT ", bits( io.x ), bits( io.y ), bits( io.z ) );
		printf( "%s %s %s ", bits( a.x ), bits( a.y ), bits( a.z ) );
		printf( "%s %s %s ", bits( b.x ), bits( b.y ), bits( b.z ) );
		printf( "%s %s %s FLG %08x DST %s\n", bits( c.x ), bits( c.y ),
			bits( c.z ), unsigned( triangle.flags() ), bits( dist ) );

		return this->say_;
	}

	void setSay( int say )		{ this->say_ = say; }
	void reset()				{ this->visits_ = 0; }
	int visits() const			{ return this->visits_; }

private:
	const char * tag_;
	int visits_;
	int say_;
};

// ---------------------------------------------------------------------------
// probe runners - local geometry in, world sweep out (via the chunk
// transform), then straight into the original collideIntoObject
// ---------------------------------------------------------------------------

static void run_ray( const char * tag, int say, const Matrix & chunkXform,
	const Vector3 & ls, const Vector3 & le, TruthCallback & cb,
	PortalObstacle * ob )
{
	cb.setSay( say );
	cb.reset();

	Vector3 ws = chunkXform.applyPoint( ls );
	Vector3 we = chunkXform.applyPoint( le );

	printf( "RAY %s LOC %s %s %s %s %s %s WRD %s %s %s ",
		tag, bits( ls.x ), bits( ls.y ), bits( ls.z ),
		bits( le.x ), bits( le.y ), bits( le.z ),
		bits( ws.x ), bits( ws.y ), bits( ws.z ) );
	printf( "%s %s %s SAY %d\n",
		bits( we.x ), bits( we.y ), bits( we.z ), say );

	SweepShape< Vector3 > start( ws );
	SweepParams sp( start, we );

	CollisionState cs( cb );
	bool stop = SweepHelpers::collideIntoObject( start, sp, *ob, ob->bb_,
		ob->transformInverse_, cs );

	printf( "RAY %s RET %d DST %s STR %s ETR %s VIS %d\n",
		tag, stop ? 1 : 0, bits( cs.dist_ ), bits( cs.sTravel_ ),
		bits( cs.eTravel_ ), cb.visits() );
}

static void run_prism( const char * tag, int say, const Matrix & chunkXform,
	const Vector3 & la, const Vector3 & lb, const Vector3 & lc,
	const Vector3 & lend, TruthCallback & cb, PortalObstacle * ob )
{
	cb.setSay( say );
	cb.reset();

	Vector3 wa = chunkXform.applyPoint( la );
	Vector3 wb = chunkXform.applyPoint( lb );
	Vector3 wc = chunkXform.applyPoint( lc );
	Vector3 wend = chunkXform.applyPoint( lend );

	printf( "PRS %s LOC %s %s %s %s %s %s %s %s %s END %s %s %s WRD ",
		tag,
		bits( la.x ), bits( la.y ), bits( la.z ),
		bits( lb.x ), bits( lb.y ), bits( lb.z ),
		bits( lc.x ), bits( lc.y ), bits( lc.z ),
		bits( lend.x ), bits( lend.y ), bits( lend.z ) );
	printf( "%s %s %s %s %s %s %s %s %s END %s %s %s SAY %d\n",
		bits( wa.x ), bits( wa.y ), bits( wa.z ),
		bits( wb.x ), bits( wb.y ), bits( wb.z ),
		bits( wc.x ), bits( wc.y ), bits( wc.z ),
		bits( wend.x ), bits( wend.y ), bits( wend.z ), say );

	SweepShape< WorldTriangle > start(
		WorldTriangle( wa, wb, wc, 0 ) );
	SweepParams sp( start, wend );

	CollisionState cs( cb );
	bool stop = SweepHelpers::collideIntoObject( start, sp, *ob, ob->bb_,
		ob->transformInverse_, cs );

	printf( "PRS %s RET %d DST %s STR %s ETR %s VIS %d\n",
		tag, stop ? 1 : 0, bits( cs.dist_ ), bits( cs.sTravel_ ),
		bits( cs.eTravel_ ), cb.visits() );
}

// ---------------------------------------------------------------------------
// portal dump
// ---------------------------------------------------------------------------

static void dump_portal( const char * tag, ChunkBoundary::Portal & p )
{
	printVec( (BW::string( tag ) + " PLN").c_str(), p.plane.normal() );
	{
		char line[64];
		snprintf( line, sizeof( line ), "%s PLD", tag );
		printf( "%s %s\n", line, bits( p.plane.d() ) );
	}
	printVec( (BW::string( tag ) + " UAX").c_str(), p.uAxis );
	printVec( (BW::string( tag ) + " VAX").c_str(), p.vAxis );
	printVec( (BW::string( tag ) + " ORG").c_str(), p.origin );
	for (BW::vector< Vector2 >::size_type i = 0; i < p.points.size(); i++)
	{
		printf( "%s PT %u %s %s\n", tag, unsigned( i ),
			bits( p.points[ i ].x ), bits( p.points[ i ].y ) );
	}
	printf( "%s PPM %d\n", tag, p.permissive ? 1 : 0 );
}

// ---------------------------------------------------------------------------
// the battery
// ---------------------------------------------------------------------------

int main()
{
	setvbuf( stdout, NULL, _IONBF, 0 );
	// chunk mapping: scale 2, then rotY 90, then translate - the mapper is
	// what an outside chunk's transform is built from (chunk.cpp ctor:
	// transform_.setTranslate(xf, 0, zf); postMultiply(mapper)).
	Matrix mScale;
	mScale.setScale( 2.f, 2.f, 2.f );
	Matrix mRot;
	mRot.setRotateY( MATH_PI * 0.5f );
	Matrix mTrans;
	mTrans.setTranslate( 10.f, 0.f, 5.f );

	Matrix mapper( mTrans );
	mapper.postMultiply( mRot );
	mapper.postMultiply( mScale );
	printMat( "MPR", mapper );

	ChunkSpacePtr space = new ChunkSpace( 1 );

	DataSectionPtr pSettings = new XMLSection( "settings" );
	GeometryMapping * pMapping = new GeometryMapping( space, mapper, "",
		pSettings );

	// An outside chunk ("1_1") skips the shell-model handling in
	// addStaticItem; its transform is grid-translate * mapper.
	Chunk chunk( "1_1o", pMapping );

	printMat( "CXT", chunk.transform() );

	// ---- rectangular portal on the chunk-local x = 0 plane ----
	PlaneEq planeRect( Vector3( 1.f, 0.f, 0.f ), 0.f );
	BW::vector< Vector3 > rectPts;
	rectPts.push_back( Vector3( -2.f, -1.f, 0.f ) );
	rectPts.push_back( Vector3( 2.f, -1.f, 0.f ) );
	rectPts.push_back( Vector3( 2.f, 3.f, 0.f ) );
	rectPts.push_back( Vector3( -2.f, 3.f, 0.f ) );

	ChunkBoundary::Portal portalRect( planeRect, Vector3( 0.f, 0.f, 1.f ),
		rectPts, pMapping, "" );
	dump_portal( "PT1", portalRect );

	PortalChunkItem itemRect( &portalRect );
	itemRect.toss( &chunk );

	PortalObstacle obstacleRect( &itemRect );
	printMat( "OBT", obstacleRect.transform_ );
	printMat( "OBI", obstacleRect.transformInverse_ );
	printVec( "OBBMIN", obstacleRect.bb_.minBounds() );
	printVec( "OBBMAX", obstacleRect.bb_.maxBounds() );
	{
		Vector3 p( -5.f, 0.f, 0.f );
		Vector3 w = chunk.transform().applyPoint( p );
		Vector3 b = obstacleRect.transformInverse_.applyPoint( w );
		Vector3 s = obstacleRect.transform_.applyPoint( p );
		printVec( "RT1F", w );
		printVec( "RT1B", b );
		printVec( "RT1S", s );
	}

	// ---- pentagonal portal on the chunk-local z = 0 plane (3-fan) ----
	PlaneEq planePen( Vector3( 0.f, 0.f, -1.f ), 0.f );
	BW::vector< Vector3 > penPts;
	penPts.push_back( Vector3( 0.f, -1.f, 0.f ) );
	penPts.push_back( Vector3( 2.f, 0.f, 0.f ) );
	penPts.push_back( Vector3( 1.5f, 2.f, 0.f ) );
	penPts.push_back( Vector3( 0.f, 2.5f, 0.f ) );
	penPts.push_back( Vector3( -2.f, 1.f, 0.f ) );

	ChunkBoundary::Portal portalPen( planePen, Vector3( 1.f, 0.f, 0.f ),
		penPts, pMapping, "" );
	dump_portal( "PT2", portalPen );

	PortalChunkItem itemPen( &portalPen );
	itemPen.toss( &chunk );

	PortalObstacle obstaclePen( &itemPen );

	TruthCallback cb( "PRT" );

	// the probe set, replayed unchanged under every portal state
	auto battery = [&]( int state )
	{
		printf( "STATE %d\n", state );

		// rect portal probes
		run_ray( "R1", COLLIDE_STOP, chunk.transform(),
			Vector3( -5.f, 0.f, 0.f ), Vector3( 5.f, 0.f, 0.f ),
			cb, &obstacleRect );
		run_ray( "R1A", COLLIDE_ALL, chunk.transform(),
			Vector3( -5.f, 0.f, 0.f ), Vector3( 5.f, 0.f, 0.f ),
			cb, &obstacleRect );
		run_ray( "R1B", COLLIDE_BEFORE, chunk.transform(),
			Vector3( -5.f, 0.f, 0.f ), Vector3( 5.f, 0.f, 0.f ),
			cb, &obstacleRect );
		run_ray( "R1C", COLLIDE_AFTER, chunk.transform(),
			Vector3( -5.f, 0.f, 0.f ), Vector3( 5.f, 0.f, 0.f ),
			cb, &obstacleRect );
		run_ray( "R2", COLLIDE_STOP, chunk.transform(),
			Vector3( -5.f, 0.f, 2.5f ), Vector3( 5.f, 0.f, 2.5f ),
			cb, &obstacleRect );
		run_ray( "R3", COLLIDE_STOP, chunk.transform(),
			Vector3( -5.f, 0.5f, 1.f ), Vector3( 5.f, 0.5f, 1.f ),
			cb, &obstacleRect );
		run_ray( "R4", COLLIDE_STOP, chunk.transform(),
			Vector3( -5.f, 2.f, 0.f ), Vector3( 5.f, 2.f, 0.f ),
			cb, &obstacleRect );

		// pentagon portal probes
		run_ray( "R5", COLLIDE_STOP, chunk.transform(),
			Vector3( 0.5f, 0.5f, -3.f ), Vector3( 0.5f, 0.5f, 3.f ),
			cb, &obstaclePen );
		run_ray( "R6", COLLIDE_STOP, chunk.transform(),
			Vector3( -1.8f, 2.f, -3.f ), Vector3( -1.8f, 2.f, 3.f ),
			cb, &obstaclePen );

		// prism probes against the rect portal
		run_prism( "W1", COLLIDE_STOP, chunk.transform(),
			Vector3( -5.f, -1.f, 0.f ), Vector3( -3.f, -1.f, 0.f ),
			Vector3( -5.f, -1.f, 1.f ), Vector3( 5.f, -1.f, 0.f ),
			cb, &obstacleRect );
		run_prism( "W2", COLLIDE_STOP, chunk.transform(),
			Vector3( -5.f, -1.f, 2.5f ), Vector3( -3.f, -1.f, 2.5f ),
			Vector3( -5.f, -1.f, 3.5f ), Vector3( 5.f, -1.f, 2.5f ),
			cb, &obstacleRect );
	};

	// ---- the switch: both sides of every transition replay the battery --
	battery( 0 ); // default: permissive (open)

	printf( "PSTM rect close flags 2103\n" );
	portalRect.setCollisionState( &chunk, false,
		WorldTriangle::Flags( 0x2103 ) );
	portalPen.setCollisionState( &chunk, false,
		WorldTriangle::Flags( 0x2103 ) );
	battery( 1 ); // closed, collision flags 0x2103 (uint8-truncated)

	printf( "PSTM rect open\n" );
	portalRect.setCollisionState( &chunk, true, WorldTriangle::Flags( 0 ) );
	portalPen.setCollisionState( &chunk, true, WorldTriangle::Flags( 0 ) );
	battery( 2 ); // reopened - must match state 0 answers

	printf( "PSTM rect close flags 0\n" );
	portalRect.setCollisionState( &chunk, false, WorldTriangle::Flags( 0 ) );
	portalPen.setCollisionState( &chunk, false, WorldTriangle::Flags( 0 ) );
	battery( 3 ); // closed again with zero flags

	printf( "RESULT OK\n" );
	_exit( 0 ); // skip library teardown ordering noise
}
