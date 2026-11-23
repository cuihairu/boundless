// BW-00-04-01 — capture reference Mercury frames on the wire.
//
// This program defines a small interface the way the reference's own unit
// tests do (lib/network/unit_test/common_interface.hpp), sends one fixture
// message per case through the real Mercury::NetworkInterface to a loopback
// UDP listener, and prints the datagram bytes as hex. Every byte is produced
// by the reference stack (BigWorld 4657a17); the program only reads them
// back off the socket, so the bytes are the original stack's wire output,
// not a reimplementation.
//
// Build (flags mirror build/cmake-network/CMakeFiles/network_test.dir/
// flags.make of the reference tree):
//
//   REF=<reference checkout>
//   g++ -std=gnu++23 -O2 -DBW_BUILD_PLATFORM='"el7"' -DCODE_INLINE \
//       -DMF_CONFIG='"hybrid"' -DMF_SERVER -DMF_USE_ASSERTS -DUSE_OPENSSL \
//       -D_HYBRID -I$REF/lib -I$REF/third_party \
//       -isystem $REF/build/cmake-network/vcpkg_installed/x64-linux-bw/include \
//       capture_protocol.cpp -o /tmp/capture_protocol \
//       -L$REF/build/cmake-network \
//       -Wl,--start-group -lnetwork -lresmgr -lmath -lcstdmf -Wl,--end-group \
//       $REF/build/cmake-network/vcpkg_installed/x64-linux-bw/lib/libz.a \
//       $REF/build/cmake-network/vcpkg_installed/x64-linux-bw/lib/libssl.a \
//       $REF/build/cmake-network/vcpkg_installed/x64-linux-bw/lib/libcrypto.a \
//       -ldl -lm -lpthread

#define DEFINE_SERVER_HERE
#include "network/interface_macros.hpp"
#include "network/network_interface.hpp"

#include <arpa/inet.h>
#include <netinet/in.h>
#include <sys/socket.h>
#include <unistd.h>

#include <cstdint>
#include <cstdio>
#include <cstring>
#include <string>
#include <vector>

BW_BEGIN_NAMESPACE

// The fixture interface: four elements whose framing forms cover the whole
// Mercury message header grammar -- empty (no length field at all), fixed
// (no length field), variable with a 1-byte and a 4-byte length field.
BEGIN_MERCURY_INTERFACE( FixtureInterface )
	MERCURY_EMPTY_MESSAGE( ping, NULL )
	MERCURY_FIXED_MESSAGE( fixed4, 4, NULL )
	MERCURY_VARIABLE_MESSAGE( var1, 1, NULL )
	MERCURY_VARIABLE_MESSAGE( var2, 4, NULL )
END_MERCURY_INTERFACE()

BW_END_NAMESPACE

using namespace BW;

namespace {

const uint16_t LISTEN_PORT = 20997;

class NullReplyHandler : public Mercury::ReplyMessageHandler
{
public:
	virtual void handleMessage( const Mercury::Address &,
		Mercury::UnpackedMessageHeader &, BinaryIStream &, void * ) {}

	virtual void handleException( const Mercury::NubException &, void * ) {}
};

int open_listener()
{
	int fd = ::socket( AF_INET, SOCK_DGRAM, 0 );
	int one = 1;
	::setsockopt( fd, SOL_SOCKET, SO_REUSEADDR, &one, sizeof( one ) );

	sockaddr_in addr;
	::memset( &addr, 0, sizeof( addr ) );
	addr.sin_family = AF_INET;
	addr.sin_addr.s_addr = htonl( INADDR_LOOPBACK );
	addr.sin_port = htons( LISTEN_PORT );
	if ( ::bind( fd, (sockaddr *)&addr, sizeof( addr ) ) != 0 )
	{
		::fprintf( stderr, "capture_protocol: bind 127.0.0.1:%u failed\n",
			(unsigned)LISTEN_PORT );
		::exit( 1 );
	}
	return fd;
}

std::vector< uint8_t > receive_one( int fd )
{
	std::vector< uint8_t > buf( 65536 );
	ssize_t n = ::recvfrom( fd, buf.data(), buf.size(), 0, NULL, NULL );
	buf.resize( n < 0 ? 0 : (size_t)n );
	return buf;
}

void emit( const char * name,
	const char * message,
	unsigned id,
	const char * direction,
	const char * style,
	int lengthParam,
	const std::vector< uint8_t > & bytes )
{
	printf( "case %s\n", name );
	printf( "  interface FixtureInterface\n" );
	printf( "  message %s\n", message );
	printf( "  id %u\n", id );
	printf( "  direction %s\n", direction );
	printf( "  style %s\n", style );
	printf( "  lengthParam %d\n", lengthParam );
	printf( "  bytes" );
	for ( uint8_t b : bytes )
	{
		printf( " %02x", b );
	}
	printf( "\n\n" );
}

} // anonymous namespace

int main()
{
	int listener = open_listener();

	Mercury::NetworkInterface interface_( NULL,
		Mercury::NETWORK_INTERFACE_EXTERNAL );
	Mercury::Address dest( htonl( INADDR_LOOPBACK ), htons( LISTEN_PORT ) );
	printf( "interface FixtureInterface\n" );
	printf( "  ping id %u\n", (unsigned)FixtureInterface::ping.id() );
	printf( "  fixed4 id %u\n", (unsigned)FixtureInterface::fixed4.id() );
	printf( "  var1 id %u\n", (unsigned)FixtureInterface::var1.id() );
	printf( "  var2 id %u\n", (unsigned)FixtureInterface::var2.id() );
	printf( "\n" );

	NullReplyHandler replyHandler;

	// 1. Notification, empty message: header byte only.
	{
		Mercury::UDPBundle bundle;
		bundle.startMessage( FixtureInterface::ping, Mercury::RELIABLE_NO );
		bundle.finalise();
		interface_.send( dest, bundle );
		emit( "notification-empty", "ping",
			(unsigned)FixtureInterface::ping.id(), "client->server",
			"EMPTY_MESSAGE", 0, receive_one( listener ) );
	}

	// 2. Notification, fixed 4-byte payload: no length field.
	{
		Mercury::UDPBundle bundle;
		bundle.startMessage( FixtureInterface::fixed4, Mercury::RELIABLE_NO );
		bundle << (uint8)1 << (uint8)2 << (uint8)3 << (uint8)4;
		bundle.finalise();
		interface_.send( dest, bundle );
		emit( "notification-fixed", "fixed4",
			(unsigned)FixtureInterface::fixed4.id(), "client->server",
			"FIXED_LENGTH_MESSAGE", 4, receive_one( listener ) );
	}

	// 3. Notification, variable 1-byte length, short body.
	{
		Mercury::UDPBundle bundle;
		bundle.startMessage( FixtureInterface::var1, Mercury::RELIABLE_NO );
		bundle << (uint8)'A' << (uint8)'B' << (uint8)'C' << (uint8)'D';
		bundle.finalise();
		interface_.send( dest, bundle );
		emit( "notification-var1-short", "var1",
			(unsigned)FixtureInterface::var1.id(), "client->server",
			"VARIABLE_LENGTH_MESSAGE", 1, receive_one( listener ) );
	}

	// 4. Notification, variable 1-byte length, long body: crosses the
	// one-byte threshold, so the length field takes the special form.
	{
		Mercury::UDPBundle bundle;
		bundle.startMessage( FixtureInterface::var1, Mercury::RELIABLE_NO );
		for ( int i = 0; i < 300; ++i )
		{
			bundle << (uint8)0xAA;
		}
		bundle.finalise();
		interface_.send( dest, bundle );
		emit( "notification-var1-long", "var1",
			(unsigned)FixtureInterface::var1.id(), "client->server",
			"VARIABLE_LENGTH_MESSAGE", 1, receive_one( listener ) );
	}

	// 5. Notification, variable 4-byte length field.
	{
		Mercury::UDPBundle bundle;
		bundle.startMessage( FixtureInterface::var2, Mercury::RELIABLE_NO );
		bundle << (uint8)1 << (uint8)2 << (uint8)3 << (uint8)4
			<< (uint8)5 << (uint8)6 << (uint8)7 << (uint8)8;
		bundle.finalise();
		interface_.send( dest, bundle );
		emit( "notification-var2", "var2",
			(unsigned)FixtureInterface::var2.id(), "client->server",
			"VARIABLE_LENGTH_MESSAGE", 4, receive_one( listener ) );
	}

	// 6. Request: same message, request form -- the reply ID is stamped by
	// NetworkInterface::send() (UDPBundle::startRequest reserves it).
	{
		Mercury::UDPBundle bundle;
		bundle.startRequest( FixtureInterface::var1, &replyHandler, NULL,
			500000 /* 0.5s in stamps */, Mercury::RELIABLE_NO );
		bundle << (uint8)'A' << (uint8)'B' << (uint8)'C' << (uint8)'D';
		bundle.finalise();
		interface_.send( dest, bundle );
		emit( "request-var1", "var1",
			(unsigned)FixtureInterface::var1.id(), "client->server",
			"VARIABLE_LENGTH_MESSAGE", 1, receive_one( listener ) );
	}

	// 7. Reply: startReply writes the reply preamble.
	{
		Mercury::UDPBundle bundle;
		bundle.startReply( (Mercury::ReplyID)7, Mercury::RELIABLE_NO );
		bundle << (uint8)'O' << (uint8)'K';
		bundle.finalise();
		interface_.send( dest, bundle );
		emit( "reply-var1", "(reply)", 0, "server->client",
			"REPLY", 0, receive_one( listener ) );
	}

	::close( listener );
	return 0;
}
