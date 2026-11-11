//! The two reward presets. Any other reward is the user's, computed from
//! the view the environment returns.

use pikapro::SideId;

/// How rewards are computed. Both presets are zero-sum between the sides,
/// which suits self-play.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum RewardPreset {
    /// +1 for a win, −1 for a loss, 0 for a tie, when the battle ends. A
    /// turn-limit end is 0 and `truncated`, not `terminated`. The default.
    #[default]
    Terminal,
    /// [`Terminal`](Self::Terminal), plus `weight` (0.1 by default) per
    /// foe Pokémon knocked out and −`weight` per own. Computed as the
    /// change in Pokémon still standing (only the brought ones with team
    /// preview), so a Revival Blessing takes a KO back.
    ///
    /// The episode's return is the result plus `weight` times the final KO
    /// margin (foes knocked out minus own): a bigger win is worth more.
    /// This is a KO-margin bonus, not policy-invariant potential shaping
    /// (the potential isn't zeroed at the end), so it changes the
    /// objective: the agent maximises its win chance plus the expected
    /// margin.
    Ko { weight: f32 },
}

impl RewardPreset {
    /// [`RewardPreset::Ko`] with the default weight.
    pub const KO: RewardPreset = RewardPreset::Ko { weight: 0.1 };

    /// The terminal part of side `me`'s reward for a battle won by
    /// `winner` (`None`: a tie).
    pub fn terminal(me: SideId, winner: Option<SideId>) -> f32 {
        match winner {
            Some(w) if w == me => 1.0,
            Some(_) => -1.0,
            None => 0.0,
        }
    }

    /// The shaping part of side `me`'s reward for a step in which the
    /// sides' Pokémon still standing went from `before` to `after` (by
    /// side index).
    pub fn shaping(self, me: SideId, before: [u8; 2], after: [u8; 2]) -> f32 {
        match self {
            RewardPreset::Terminal => 0.0,
            RewardPreset::Ko { weight } => {
                let lost = |s: SideId| before[s.index()] as f32 - after[s.index()] as f32;
                weight * (lost(me.foe()) - lost(me))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_are_zero_sum() {
        for winner in [Some(SideId::P1), Some(SideId::P2), None] {
            assert_eq!(RewardPreset::terminal(SideId::P1, winner) + RewardPreset::terminal(SideId::P2, winner), 0.0);
        }
        let ko = RewardPreset::KO;
        let (before, after) = ([4, 4], [3, 2]);
        let p1 = ko.shaping(SideId::P1, before, after);
        assert!((p1 - 0.1).abs() < 1e-6, "{p1}");
        assert_eq!(p1 + ko.shaping(SideId::P2, before, after), 0.0);
        // A revival takes a KO back.
        assert!(ko.shaping(SideId::P1, [2, 2], [3, 2]) > 0.0);
        assert_eq!(RewardPreset::Terminal.shaping(SideId::P1, before, after), 0.0);
    }
}
