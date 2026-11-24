//! BW-10-03-04: the collision triangle (`lib/physics2/worldtri.hpp`).
//!
//! A triangle is three `Vector3` vertices plus a 16-bit flags word: the
//! low byte carries the collision flags, the high byte the material kind
//! (`worldtri.hpp:30-49`). On disk it is a raw 40-byte POD row (36 bytes
//! of vertices, flags, padding — `bsp.cpp:1188-1191`).

use foundation::math::Vector3;

/// `TRIANGLE_NOT_IN_BSP` — nothing ever collides with this triangle.
pub const TRIANGLE_NOT_IN_BSP: u16 = 0xff;
pub const TRIANGLE_CAMERANOCOLLIDE: u16 = 1 << 0;
pub const TRIANGLE_TRANSPARENT: u16 = 1 << 1;
pub const TRIANGLE_BLENDED: u16 = 1 << 2;
pub const TRIANGLE_TERRAIN: u16 = 1 << 3;
/// The player does not collide; the camera still does.
pub const TRIANGLE_NOCOLLIDE: u16 = 1 << 4;
pub const TRIANGLE_DOUBLESIDED: u16 = 1 << 5;
pub const TRIANGLE_WATER: u16 = 1 << 6;
pub const TRIANGLE_PROJECTILENOCOLLIDE: u16 = 1 << 7;

pub const TRIANGLE_COLLISIONFLAG_MASK: u16 = 0xff;
pub const TRIANGLE_MATERIALKIND_MASK: u16 = 0xff << 8;
/// How many bits to shift to get to the material-kind byte.
pub const TRIANGLE_MATERIALKIND_SHIFT: u32 = 8;

/// The convenience "do not collide against" flags
/// (`worldtri.hpp:139-140`).
pub const BW_TRIANGLE_NOCOLLIDE: u16 = TRIANGLE_NOCOLLIDE | TRIANGLE_WATER;

/// `sizeof(WorldTriangle)` — the on-disk row size the BSP format relies
/// on (`bsp.cpp:1188-1191`).
pub const WORLD_TRIANGLE_SIZE: usize = 40;

/// A triangle in the collision scene.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorldTriangle {
    v: [Vector3; 3],
    flags: u16,
}

impl WorldTriangle {
    pub fn new(v0: Vector3, v1: Vector3, v2: Vector3, flags: u16) -> Self {
        Self {
            v: [v0, v1, v2],
            flags,
        }
    }

    pub fn v0(&self) -> Vector3 {
        self.v[0]
    }

    pub fn v1(&self) -> Vector3 {
        self.v[1]
    }

    pub fn v2(&self) -> Vector3 {
        self.v[2]
    }

    /// `v(index)` — `MF_ASSERT(index < 3)` (`worldtri.ipp:85-89`).
    pub fn v(&self, index: u32) -> Vector3 {
        assert!(index < 3, "reference MF_ASSERTs index < 3");
        self.v[index as usize]
    }

    /// `normal()` (`worldtri.ipp:95-98`): `(v2 - v0) x (v2 - v1)` — the
    /// reference vertex order, not the usual `(v1-v0) x (v2-v0)`.
    pub fn normal(&self) -> Vector3 {
        Vector3::ZERO.cross_product(&(self.v[2] - self.v[0]), &(self.v[2] - self.v[1]))
    }

    pub fn flags(&self) -> u16 {
        self.flags
    }

    pub fn set_flags(&mut self, new_flags: u16) {
        self.flags = new_flags;
    }

    /// `collisionFlags(flags)` — the low byte (`worldtri.hpp:81-82`).
    pub fn collision_flags(&self) -> u16 {
        self.flags & TRIANGLE_COLLISIONFLAG_MASK
    }

    /// `materialKind(flags)` — the high byte (`worldtri.hpp:83-84`).
    pub fn material_kind(&self) -> u16 {
        (self.flags & TRIANGLE_MATERIALKIND_MASK) >> TRIANGLE_MATERIALKIND_SHIFT
    }

    /// `packFlags` — compose a flags word from the two bytes.
    pub fn pack_flags(collision_flags: i32, material_kind: i32) -> u16 {
        ((collision_flags & TRIANGLE_COLLISIONFLAG_MASK as i32)
            | (material_kind << TRIANGLE_MATERIALKIND_SHIFT)) as u16
    }

    /// Decode one 40-byte on-disk row: three vertices, flags, padding
    /// (padding is not kept — the reference `operator==` compares it, the
    /// port treats rows as equal when the visible fields are equal).
    pub fn from_row(row: &[u8]) -> Self {
        let vf = |i: usize| {
            Vector3::new(
                f32::from_le_bytes(row[i * 12..i * 12 + 4].try_into().unwrap()),
                f32::from_le_bytes(row[i * 12 + 4..i * 12 + 8].try_into().unwrap()),
                f32::from_le_bytes(row[i * 12 + 8..i * 12 + 12].try_into().unwrap()),
            )
        };
        Self {
            v: [vf(0), vf(1), vf(2)],
            flags: u16::from_le_bytes(row[36..38].try_into().unwrap()),
        }
    }

    /// Encode one 40-byte on-disk row (padding zeroed).
    pub fn to_row(&self) -> [u8; WORLD_TRIANGLE_SIZE] {
        let mut row = [0u8; WORLD_TRIANGLE_SIZE];
        for (i, v) in self.v.iter().enumerate() {
            row[i * 12..i * 12 + 4].copy_from_slice(&v.x.to_le_bytes());
            row[i * 12 + 4..i * 12 + 8].copy_from_slice(&v.y.to_le_bytes());
            row[i * 12 + 8..i * 12 + 12].copy_from_slice(&v.z.to_le_bytes());
        }
        row[36..38].copy_from_slice(&self.flags.to_le_bytes());
        row
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::panic::{AssertUnwindSafe, catch_unwind};

    fn v(x: f32, y: f32, z: f32) -> Vector3 {
        Vector3::new(x, y, z)
    }

    #[test]
    fn the_row_codec_round_trips_and_ignores_padding() {
        let tri = WorldTriangle::new(
            v(1.0, 2.0, 3.0),
            v(4.0, 5.0, 6.0),
            v(7.0, 8.0, 10.0),
            0x2A05,
        );
        let row = tri.to_row();
        assert_eq!(row.len(), WORLD_TRIANGLE_SIZE);
        assert_eq!(&row[38..40], &[0, 0], "padding is zeroed on encode");
        // Padding bytes on disk are not part of the port's equality.
        let mut padded = row;
        padded[38] = 0xab;
        padded[39] = 0xcd;
        assert_eq!(WorldTriangle::from_row(&padded), tri);
        assert_eq!(WorldTriangle::from_row(&row).to_row(), row);
    }

    #[test]
    fn normal_follows_the_reference_cross_order() {
        let tri = WorldTriangle::new(v(1.0, 2.0, 3.0), v(4.0, 5.0, 6.0), v(7.0, 8.0, 10.0), 0);
        // (v2 - v0) x (v2 - v1), the reference's vertex order.
        let a = v(6.0, 6.0, 7.0);
        let b = v(3.0, 3.0, 4.0);
        let expected = Vector3::ZERO.cross_product(&a, &b);
        assert_eq!(tri.normal(), expected);
        assert_eq!(expected.x, 3.0);
        assert_eq!(expected.y, -3.0);
        assert_eq!(expected.z, 0.0);
    }

    #[test]
    fn vertex_accessors_and_the_indexed_one_match_the_reference() {
        let tri = WorldTriangle::new(v(1.0, 1.0, 1.0), v(2.0, 2.0, 2.0), v(3.0, 3.0, 3.0), 0);
        assert_eq!(tri.v0(), v(1.0, 1.0, 1.0));
        assert_eq!(tri.v1(), v(2.0, 2.0, 2.0));
        assert_eq!(tri.v2(), v(3.0, 3.0, 3.0));
        assert_eq!(tri.v(0), tri.v0());
        assert_eq!(tri.v(1), tri.v1());
        assert_eq!(tri.v(2), tri.v2());
        assert!(catch_unwind(AssertUnwindSafe(|| tri.v(3))).is_err());
    }

    #[test]
    fn the_flags_word_splits_into_two_bytes() {
        let mut tri = WorldTriangle::new(v(0.0, 0.0, 0.0), v(1.0, 0.0, 0.0), v(0.0, 1.0, 0.0), 0);
        assert_eq!(tri.flags(), 0);
        assert_eq!(tri.collision_flags(), 0);
        assert_eq!(tri.material_kind(), 0);

        tri.set_flags(0x2A05);
        assert_eq!(tri.flags(), 0x2A05);
        assert_eq!(tri.collision_flags(), 0x05);
        assert_eq!(tri.material_kind(), 0x2A);
        assert_eq!(WorldTriangle::pack_flags(0x05, 0x2A), 0x2A05);
        // packFlags masks the collision byte like the reference.
        assert_eq!(WorldTriangle::pack_flags(0x105, 0x2A), 0x2A05);
    }

    #[test]
    fn the_nocollide_pair_combines_player_and_water_bits() {
        assert_eq!(BW_TRIANGLE_NOCOLLIDE, TRIANGLE_NOCOLLIDE | TRIANGLE_WATER);
        assert_eq!(BW_TRIANGLE_NOCOLLIDE, 0x50);
        assert_eq!(TRIANGLE_NOT_IN_BSP, 0xff);
    }
}
