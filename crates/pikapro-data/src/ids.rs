//! Compact integer identifiers for data-table entries.

macro_rules! id_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        // `Default` (index 0) exists so IDs can live in fixed-capacity inline
        // containers; it carries no meaning.
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(pub u16);

        impl $name {
            /// The raw table index.
            #[inline]
            pub const fn index(self) -> usize {
                self.0 as usize
            }
        }
    };
}

id_type!(
    /// Index into [`Dex::species`](crate::Dex::species).
    SpeciesId
);
id_type!(
    /// Index into [`Dex::moves`](crate::Dex::moves).
    MoveId
);
id_type!(
    /// Index into [`Dex::abilities`](crate::Dex::abilities). `AbilityId(0)` is "No Ability".
    AbilityId
);
id_type!(
    /// Index into [`Dex::items`](crate::Dex::items).
    ItemId
);

impl AbilityId {
    /// The placeholder "no ability" entry, always at index 0.
    pub const NONE: AbilityId = AbilityId(0);
}
