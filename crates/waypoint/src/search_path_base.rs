//! `lib/waypoint/search_path_base.hpp` — the reverse-path base both
//! searched paths share: the current/next/destination views, `pop`, and
//! the `matches` cache test.
//!
//! The reference models this as a CRTP base with two virtuals
//! (`statesAreEquivalent`, `isPartOfPath`); the port spreads them over a
//! trait with default bodies in their place.
//!
//! Registered quirk: `matches`'s final comment says "clear it now just for
//! sanity", but the reference clears nothing there — reproduced.

/// `SearchPathBase<SearchState, SearchData>`
/// (`search_path_base.hpp:17-165`): the reverse path storage is the
/// implementor's; everything else is provided.
pub trait SearchPathBase {
    /// `SearchState`.
    type State: Clone;

    /// `reversePath_`.
    fn reverse_path(&self) -> &[Self::State];
    /// `reversePath_.clear()`.
    fn clear_reverse_path(&mut self);
    /// `statesAreEquivalent` (virtual, per subclass).
    fn states_are_equivalent(&self, s1: &Self::State, s2: &Self::State) -> bool;
    /// `isPartOfPath` (virtual, base answers false).
    fn is_part_of_path(&self, _current: &Self::State) -> bool {
        false
    }

    /// `empty()`.
    fn empty(&self) -> bool {
        self.reverse_path().is_empty()
    }

    /// `isValid()`: at least a current state and the destination state.
    fn is_valid(&self) -> bool {
        self.reverse_path().len() >= 2
    }

    /// `pCurrent()`.
    fn p_current(&self) -> Option<&Self::State> {
        self.reverse_path().last()
    }

    /// `pNext()`.
    fn p_next(&self) -> Option<&Self::State> {
        self.reverse_path()
            .len()
            .checked_sub(2)
            .and_then(|index| self.reverse_path().get(index))
    }

    /// `pDest()`.
    fn p_dest(&self) -> Option<&Self::State> {
        self.reverse_path().first()
    }

    /// `clear()`.
    fn clear(&mut self) {
        self.clear_reverse_path();
    }

    /// `pop()`.
    fn pop(&mut self) {
        if self.is_valid() {
            self.pop_back();
        }
    }

    /// `reversePath_.pop_back()` behind `pop`.
    fn pop_back(&mut self);

    /// `isNextState` (private in the reference).
    fn is_next_state(&self, current: &Self::State) -> bool {
        self.is_valid()
            && self
                .p_next()
                .is_some_and(|next| self.states_are_equivalent(current, next))
    }

    /// `isDestinationState()`.
    fn is_destination_state(&self, dest: &Self::State) -> bool {
        self.is_valid()
            && self
                .p_dest()
                .is_some_and(|end| self.states_are_equivalent(dest, end))
    }

    /// `isCurrentState()`.
    fn is_current_state(&self, current: &Self::State) -> bool {
        self.is_valid()
            && self
                .p_current()
                .is_some_and(|here| self.states_are_equivalent(current, here))
    }

    /// `matches` (`search_path_base.hpp:174-208`): destination must match,
    /// and the source must be the path's next, current, or any member
    /// state — advancing over `pop` in the next case.
    fn matches(&mut self, src: &Self::State, dst: &Self::State) -> bool {
        if !self.is_destination_state(dst) {
            // Destination has changed.
            return false;
        }

        // OK, this path leads to the right place. Now make sure it starts
        // from the right place too.
        if self.is_next_state(src) {
            // Advance up the path.
            self.pop();
            return true;
        } else if self.is_current_state(src) {
            // Looks good.
            return true;
        } else if self.is_part_of_path(src) {
            return true;
        }

        // nope, no good. (The reference's "clear it now just for sanity"
        // comment clears nothing.)
        false
    }
}
