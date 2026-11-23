//! `lib/network/circular_array.hpp` -- the dense power-of-two ring the send
//! window sits in (BW-02-03-03).
//!
//! Slots are addressed by `index & mask`, so a sequence number may be used as
//! an index directly. That is the whole trick: a slot's index survives every
//! doubling, which is what lets the send window grow past `windowSize_`
//! without moving any entry -- and what lets sequence arithmetic wrap
//! straight through `SEQ_SIZE` with no special casing.

/// `CircularArray` (circular_array.hpp:27). "A template class to wrap a
/// circular array of various sizes, as long as the size is a power of two."
pub struct CircularArray<T> {
    data: Vec<T>,
    /// `mask_`: size - 1.
    mask: usize,
}

impl<T: Default> CircularArray<T> {
    /// `CircularArray( size )`: `size` zero-initialised slots.
    pub fn new(size: usize) -> Self {
        assert!(
            size.is_power_of_two() && size > 0,
            "CircularArray: size must be a power of two"
        );
        Self {
            // `vec![T::default(); size]` would demand `Clone`; `from_fn` value-initialises
            // like the reference's `new T[size]()`.
            data: Vec::from_fn(size, |_| T::default()),
            mask: size - 1,
        }
    }

    /// `size()`.
    pub fn size(&self) -> usize {
        self.mask + 1
    }

    /// `operator[]` (circular_array.hpp:34): the slot `n` lands on, masked.
    pub fn get(&self, n: usize) -> &T {
        &self.data[n & self.mask]
    }

    /// `operator[]` (circular_array.hpp:35).
    pub fn get_mut(&mut self, n: usize) -> &mut T {
        &mut self.data[n & self.mask]
    }

    /// `swap`.
    pub fn swap(&mut self, other: &mut Self) {
        std::mem::swap(&mut self.data, &mut other.data);
        std::mem::swap(&mut self.mask, &mut other.mask);
    }

    /// `doubleSize( startIndex )` (circular_array.hpp:63): double, carrying
    /// the old window's entries forward at the same absolute offsets. Every
    /// other slot comes back default, so an entry not in the carried span is
    /// dropped rather than re-indexed.
    pub fn double_size(&mut self, start_index: usize) {
        let old_size = self.size();
        let mut new_window = Self::new(old_size * 2);
        for i in 0..old_size {
            *new_window.get_mut(start_index + i) = std::mem::take(self.get_mut(start_index + i));
        }
        self.swap(&mut new_window);
    }

    /// `inflateToAtLeast( newSize )` (circular_array.hpp:48): double until the
    /// request fits, then swap in the empty array. Note this does *not* carry
    /// the old contents forward -- its only caller is the channel's state
    /// deserialization, which re-fills the ring slot by slot afterwards
    /// (udp_channel.cpp:484-499).
    pub fn inflate_to_at_least(&mut self, new_size: usize) {
        if new_size > self.size() {
            let mut size = self.size();
            while new_size > size {
                size *= 2;
            }
            let mut new_window = Self::new(size);
            self.swap(&mut new_window);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every carried slot keeps its absolute index across a doubling. That is
    /// the property the send window needs: it doubles with `oldestUnackedSeq_`
    /// as the start, so every live entry's slot survives unmoved.
    #[test]
    fn doubling_keeps_a_slot_at_its_absolute_index() {
        let mut ring: CircularArray<i32> = CircularArray::new(4);
        *ring.get_mut(2) = 2;
        *ring.get_mut(3) = 3;

        ring.double_size(2);

        assert_eq!(ring.size(), 8);
        assert_eq!(ring.get(2), &2, "carried slots keep their index");
        assert_eq!(ring.get(3), &3);
    }

    /// The carried span is `[start, start + size)`; anything outside it is
    /// dropped, not relocated.
    #[test]
    fn doubling_carries_only_the_live_span() {
        let mut ring: CircularArray<i32> = CircularArray::new(2);
        // Both slots are live here: the span is the whole window.
        *ring.get_mut(3) = 3;
        *ring.get_mut(4) = 4;

        ring.double_size(3);

        assert_eq!(ring.get(3), &3);
        assert_eq!(ring.get(4), &4);
        assert_eq!(ring.get(5), &0, "outside the span comes back default");

        let mut empty: CircularArray<i32> = CircularArray::new(2);
        *empty.get_mut(0) = 1;
        empty.double_size(1);
        assert_eq!(empty.get(0), &0, "a slot before the span is dropped");
    }

    /// A slot that was never set stays default.
    #[test]
    fn unset_slots_are_default() {
        let ring: CircularArray<Option<u32>> = CircularArray::new(8);
        assert!(ring.get(0).is_none());
        assert!(ring.get(9).is_none());
    }

    /// `inflateToAtLeast` only grows, never shrinks, and rounds up to the next
    /// power of two -- and it discards the contents it grows away.
    #[test]
    fn inflating_grows_to_the_next_power_of_two() {
        let mut ring: CircularArray<i32> = CircularArray::new(4);
        ring.inflate_to_at_least(4);
        assert_eq!(ring.size(), 4, "a request already met does nothing");

        ring.inflate_to_at_least(5);
        assert_eq!(ring.size(), 8, "rounds up");

        *ring.get_mut(3) = 3;
        ring.inflate_to_at_least(32);
        assert_eq!(ring.size(), 32);
        assert_eq!(ring.get(3), &0, "the grow-away drops the contents");

        ring.inflate_to_at_least(1);
        assert_eq!(ring.size(), 32, "it never shrinks");
    }

    /// `swap` exchanges both the slots and the mask.
    #[test]
    fn swapping_trades_contents_and_size() {
        let mut small: CircularArray<i32> = CircularArray::new(2);
        let mut big: CircularArray<i32> = CircularArray::new(16);
        *small.get_mut(0) = 1;
        *big.get_mut(0) = 2;

        small.swap(&mut big);

        assert_eq!(small.size(), 16);
        assert_eq!(small.get(0), &2);
        assert_eq!(big.size(), 2);
        assert_eq!(big.get(0), &1);
    }

    /// The size is a power of two or the mask is wrong: refuse.
    #[test]
    #[should_panic]
    fn a_non_power_of_two_size_refuses() {
        let _ring: CircularArray<i32> = CircularArray::new(3);
    }
}
