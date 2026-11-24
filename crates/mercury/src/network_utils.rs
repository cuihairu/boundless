//! `lib/network/network_utils.cpp`: the kernel socket-buffer maxima
//! behind the interface's wanted sizes.
//!
//! `getMaxBufferSize` reads `/proc/sys/net/core/{r,w}mem_max`, falling
//! back to the reference's minimum constants when the entry cannot be
//! read or parsed (basictypes.hpp:78-79).

/// `MIN_SND_SKT_BUF_SIZE` (basictypes.hpp:78).
pub const MIN_SND_SKT_BUF_SIZE: i32 = 1_048_576;

/// `MIN_RCV_SKT_BUF_SIZE` (basictypes.hpp:79).
pub const MIN_RCV_SKT_BUF_SIZE: i32 = 16_777_216;

/// `getMaxBufferSize` (network_utils.cpp:91): the kernel maximum for
/// the receive or send path, from `/proc/sys/net/core/`.
pub fn get_max_buffer_size(is_read_buffer: bool) -> i32 {
    let (path, default) = if is_read_buffer {
        ("/proc/sys/net/core/rmem_max", MIN_RCV_SKT_BUF_SIZE)
    } else {
        ("/proc/sys/net/core/wmem_max", MIN_SND_SKT_BUF_SIZE)
    };
    get_max_buffer_size_at(path, default)
}

/// The open-and-parse half of `getMaxBufferSize`, split so tests can
/// drive both fallback arms. The reference keeps the default when the
/// entry cannot be read (network_utils.cpp:113-116) or parsed
/// (the BIGWORLD 3.13 migration comment keeps the default there too).
fn get_max_buffer_size_at(path: &str, default: i32) -> i32 {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) => {
            eprintln!(
                "Unable to read buffer size from: {}, error:{}",
                path,
                crate::packet_receiver::strerror(err.raw_os_error().unwrap_or(0))
            );
            return default;
        }
    };
    // `fscanf("%d")`: the first whitespace-separated token as an
    // integer, default when there is none.
    match text
        .split_whitespace()
        .next()
        .and_then(|first| first.parse::<i32>().ok())
    {
        Some(size) => size,
        None => default,
    }
}

/// `NetworkInterface::recvBufferSize_`/`sendBufferSize_`
/// (network_interface.cpp:78-83): the wanted sizes, computed once at
/// first use like the reference's static initialisation.
pub fn buffer_sizes() -> (i32, i32) {
    static SIZES: std::sync::OnceLock<(i32, i32)> = std::sync::OnceLock::new();
    *SIZES.get_or_init(|| (get_max_buffer_size(true), get_max_buffer_size(false)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The kernel maxima are the wanted sizes, read as plain integers.
    #[test]
    fn the_procfs_maxima_are_the_wanted_sizes() {
        let rmem = std::fs::read_to_string("/proc/sys/net/core/rmem_max")
            .unwrap()
            .trim()
            .parse::<i32>()
            .unwrap();
        let wmem = std::fs::read_to_string("/proc/sys/net/core/wmem_max")
            .unwrap()
            .trim()
            .parse::<i32>()
            .unwrap();
        assert_eq!(get_max_buffer_size(true), rmem);
        assert_eq!(get_max_buffer_size(false), wmem);
    }

    /// An unreadable entry keeps the default, with the reference's
    /// warning (network_utils.cpp:113-116).
    #[test]
    fn an_unreadable_entry_keeps_the_default() {
        assert_eq!(
            get_max_buffer_size_at("/proc/sys/net/core/nope", 1234),
            1234
        );
    }

    /// An unparseable entry keeps the default too; a leading integer is
    /// read the way `fscanf("%d")` reads it.
    #[test]
    fn an_unparseable_entry_keeps_the_default() {
        let path = std::env::temp_dir().join("boundless-max-buffer-test");

        std::fs::write(&path, "not a number\n").unwrap();
        assert_eq!(get_max_buffer_size_at(&path.to_string_lossy(), 99), 99);

        std::fs::write(&path, "  7 junk\n").unwrap();
        assert_eq!(get_max_buffer_size_at(&path.to_string_lossy(), 99), 7);

        std::fs::remove_file(&path).unwrap();
    }

    /// The wanted sizes are computed once, like the reference's static
    /// initialisation of `recvBufferSize_`/`sendBufferSize_`.
    #[test]
    fn the_wanted_sizes_are_computed_once() {
        assert_eq!(
            buffer_sizes(),
            (get_max_buffer_size(true), get_max_buffer_size(false))
        );
    }
}
