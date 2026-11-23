//! Drives a real (reference) BigWorld message_logger over its watcher-nub
//! TCP protocol to produce original-format sample log files. The generated
//! tree is the fixture the file-log writer/reader are byte-compared against.
//!
//! Wire shape (watcher_connection.cpp / watcher_nub.cpp):
//! `[i32 size][i32 messageID][payload]` per packet, size excluding the
//! prefix. Payloads follow the ingestion protocol ported in registry.rs and
//! forwarding_handler.rs.

use message_logger::stream::MemoryOStream;
use std::io::{Read, Write};
use std::net::TcpStream;

/// The truncated literal the reference sample sent for "pi is %f"; not PI.
#[allow(clippy::approx_constant)]
const SAMPLE_PI: f64 = 3.14159;

fn i32_le(value: i32) -> [u8; 4] {
    value.to_le_bytes()
}

fn f64_le(value: f64) -> [u8; 8] {
    value.to_le_bytes()
}

fn packet(message_id: i32, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + payload.len());
    let size = (4 + payload.len()) as i32;
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&i32_le(message_id));
    out.extend_from_slice(payload);
    out
}

fn register_message(uid: u16, pid: u32, name: &str) -> Vec<u8> {
    let mut os = MemoryOStream::new();
    os.write_u8(message_logger::MESSAGE_LOGGER_VERSION);
    os.write_string(""); // loggerID: match all
    os.write_u16(uid);
    os.write_u32(pid);
    os.write_string(name);
    packet(
        message_logger::message_id::MESSAGE_LOGGER_REGISTER,
        os.data(),
    )
}

/// One log record: v10 header (comp prio, msg prio, source, category),
/// format string, pre-encoded args and an optional metadata tail.
fn log_message(
    msg_priority: u8,
    source: i32,
    category: &str,
    fmt: &str,
    args: &[u8],
    metadata: &[u8],
) -> Vec<u8> {
    let mut os = MemoryOStream::new();
    os.write_u8(0); // componentPriority: parseAndSend never assigns it
    os.write_u8(msg_priority);
    os.add_blob(&i32_le(source));
    os.write_string(category);
    os.write_string(fmt);
    os.add_blob(args);
    os.add_blob(metadata);
    packet(message_logger::message_id::MESSAGE_LOGGER_MSG, os.data())
}

/// Metadata tail builder: per key/value pair
/// `[keyType='s'][key][valueType][value]`.
fn metadata_pair_str(key: &str, value: &str) -> Vec<u8> {
    let mut os = MemoryOStream::new();
    os.write_u8(b's');
    os.write_string(key);
    os.write_u8(b's');
    os.write_string(value);
    os.data().to_vec()
}

fn metadata_pair_int(key: &str, value: i32) -> Vec<u8> {
    let mut os = MemoryOStream::new();
    os.write_u8(b's');
    os.write_string(key);
    os.write_u8(b'i');
    os.add_blob(&i32_le(value));
    os.data().to_vec()
}

fn metadata_pair_double(key: &str, value: f64) -> Vec<u8> {
    let mut os = MemoryOStream::new();
    os.write_u8(b's');
    os.write_string(key);
    os.write_u8(b'd');
    os.add_blob(&f64_le(value));
    os.data().to_vec()
}

/// Int arg (default `%d` width) and string arg, encoded the way
/// forwarding_handler.rs reads them back.
fn arg_i32(value: i32) -> Vec<u8> {
    i32_le(value).to_vec()
}

fn arg_string(value: &str) -> Vec<u8> {
    let mut os = MemoryOStream::new();
    os.write_string(value);
    os.data().to_vec()
}

fn arg_f64(value: f64) -> Vec<u8> {
    f64_le(value).to_vec()
}

fn arg_u32(value: u32) -> Vec<u8> {
    value.to_le_bytes().to_vec()
}

fn main() {
    let addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "192.168.5.188:47025".to_string());
    let mut stream = TcpStream::connect(&addr).expect("connect");
    stream.set_nodelay(true).unwrap();

    stream
        .write_all(&register_message(1000, 4242, "cellappmgr"))
        .unwrap();
    stream.flush().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(200));

    // INFO from C++ with category and metadata (int, string, double).
    let mut metadata = Vec::new();
    metadata.extend_from_slice(&metadata_pair_int("hp", 42));
    metadata.extend_from_slice(&metadata_pair_str("state", "ok"));
    metadata.extend_from_slice(&metadata_pair_double("ratio", 2.5));
    stream
        .write_all(&log_message(
            message_logger::message_priority::INFO,
            0,
            "combat",
            "damage %d",
            &arg_i32(33),
            &metadata,
        ))
        .unwrap();

    // WARNING from script, no metadata.
    stream
        .write_all(&log_message(
            message_logger::message_priority::WARNING,
            1,
            "",
            "moved to %s",
            &arg_string("spawn"),
            &[],
        ))
        .unwrap();

    // Float conversion.
    stream
        .write_all(&log_message(
            message_logger::message_priority::INFO,
            0,
            "",
            "pi is %f",
            &arg_f64(SAMPLE_PI),
            &[],
        ))
        .unwrap();

    // Multiple conversions in one line.
    let mut args = Vec::new();
    args.extend_from_slice(&arg_string("mix"));
    args.extend_from_slice(&arg_i32(-7));
    args.extend_from_slice(&arg_u32(4000000000));
    args.extend_from_slice(&arg_u32(255));
    stream
        .write_all(&log_message(
            message_logger::message_priority::NOTICE,
            0,
            "combat",
            "%s did %d damage (%u raw, %x hex)",
            &args,
            &[],
        ))
        .unwrap();

    // Flag/width/precision formats: pins the FormatData bitfield nibble
    // order (cflags low, vflags high) and the cflags truncation against the
    // reference binary's strings file.
    for (fmt, args) in [
        ("%5d", arg_i32(9)),
        ("%-5s|", arg_string("ab")),
        ("%+.2f", arg_f64(1.5)),
        ("%08.3f", arg_f64(1.5)),
        ("%#x", arg_u32(255)),
        ("%o", arg_u32(8)),
        ("%X", arg_u32(255)),
        ("%*d", arg_i32(3)),
        ("%.*f", arg_f64(1.5)),
        ("%hd", arg_i32(3)),
    ] {
        stream
            .write_all(&log_message(
                message_logger::message_priority::INFO,
                0,
                "",
                fmt,
                &args,
                &[],
            ))
            .unwrap();
    }

    stream.flush().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(500));

    // Drain any reply bytes (the logger never replies, but do not hang on
    // the read) so the peer's writes never block.
    stream
        .set_read_timeout(Some(std::time::Duration::from_millis(300)))
        .ok();
    let mut sink = [0u8; 4096];
    let _ = stream.read(&mut sink);
    let _ = stream.shutdown(std::net::Shutdown::Both);
    println!("sample messages sent to {}", addr);
}
