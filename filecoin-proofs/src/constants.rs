use std::collections::HashMap;
use std::sync::RwLock;

pub use storage_proofs_core::drgraph::BASE_DEGREE as DRG_DEGREE;
pub use storage_proofs_porep::stacked::EXP_DEGREE;

use filecoin_hashers::{poseidon::PoseidonHasher, sha256::Sha256Hasher, Hasher};
use lazy_static::lazy_static;
use storage_proofs_core::{
    merkle::{BinaryMerkleTree, DiskTree, LCTree},
    util::NODE_SIZE,
    MAX_LEGACY_POREP_REGISTERED_PROOF_ID,
};
use typenum::{U0, U2, U8};

use crate::types::UnpaddedBytesAmount;

pub const SECTOR_SIZE_2_KIB: u64 = 1 << 11;
pub const SECTOR_SIZE_4_KIB: u64 = 1 << 12;
pub const SECTOR_SIZE_16_KIB: u64 = 1 << 14;
pub const SECTOR_SIZE_32_KIB: u64 = 1 << 15;
pub const SECTOR_SIZE_8_MIB: u64 = 1 << 23;
pub const SECTOR_SIZE_16_MIB: u64 = 1 << 24;
pub const SECTOR_SIZE_512_MIB: u64 = 1 << 29;
pub const SECTOR_SIZE_1_GIB: u64 = 1 << 30;
pub const SECTOR_SIZE_32_GIB: u64 = 1 << 35;
pub const SECTOR_SIZE_64_GIB: u64 = 1 << 36;

pub const WINNING_POST_CHALLENGE_COUNT: usize = 66;
pub const WINNING_POST_SECTOR_COUNT: usize = 1;

pub const WINDOW_POST_CHALLENGE_COUNT: usize = 10;

pub const MAX_LEGACY_REGISTERED_SEAL_PROOF_ID: u64 = MAX_LEGACY_POREP_REGISTERED_PROOF_ID;

/// Constant NI-PoRep aggregation bounds specified in FIP-0090, but
/// superseded by FIP-0092
pub const FIP92_MIN_NI_POREP_AGGREGATION_PROOFS: usize = 1;
pub const FIP92_MAX_NI_POREP_AGGREGATION_PROOFS: usize = 65;

/// Sector sizes for which parameters are supported.
pub const SUPPORTED_SECTOR_SIZES: [u64; 10] = [
    SECTOR_SIZE_2_KIB,
    SECTOR_SIZE_4_KIB,
    SECTOR_SIZE_16_KIB,
    SECTOR_SIZE_32_KIB,
    SECTOR_SIZE_8_MIB,
    SECTOR_SIZE_16_MIB,
    SECTOR_SIZE_512_MIB,
    SECTOR_SIZE_1_GIB,
    SECTOR_SIZE_32_GIB,
    SECTOR_SIZE_64_GIB,
];

/// Sector sizes for which parameters have been published.
pub const PUBLISHED_SECTOR_SIZES: [u64; 5] = [
    SECTOR_SIZE_2_KIB,
    SECTOR_SIZE_8_MIB,
    SECTOR_SIZE_512_MIB,
    SECTOR_SIZE_32_GIB,
    SECTOR_SIZE_64_GIB,
];

lazy_static! {
    pub static ref POREP_PARTITIONS: RwLock<HashMap<u64, u8>> = RwLock::new(
        [
            (SECTOR_SIZE_2_KIB, 1),
            (SECTOR_SIZE_4_KIB, 1),
            (SECTOR_SIZE_16_KIB, 1),
            (SECTOR_SIZE_32_KIB, 1),
            (SECTOR_SIZE_8_MIB, 1),
            (SECTOR_SIZE_16_MIB, 1),
            (SECTOR_SIZE_512_MIB, 1),
            (SECTOR_SIZE_1_GIB, 10),
            (SECTOR_SIZE_32_GIB, 10),
            (SECTOR_SIZE_64_GIB, 10),
        ]
        .iter()
        .copied()
        .collect()
    );
    pub static ref LAYERS: RwLock<HashMap<u64, usize>> = RwLock::new(
        [
            (SECTOR_SIZE_2_KIB, 2),
            (SECTOR_SIZE_4_KIB, 2),
            (SECTOR_SIZE_16_KIB, 2),
            (SECTOR_SIZE_32_KIB, 2),
            (SECTOR_SIZE_8_MIB, 2),
            (SECTOR_SIZE_16_MIB, 2),
            (SECTOR_SIZE_512_MIB, 2),
            (SECTOR_SIZE_1_GIB, 11),
            (SECTOR_SIZE_32_GIB, 11),
            (SECTOR_SIZE_64_GIB, 11),
        ]
        .iter()
        .copied()
        .collect()
    );
    // These numbers must match those used for Window PoSt scheduling in the miner actor.
    // Please coordinate changes with actor code.
    // https://github.com/filecoin-project/specs-actors/blob/master/actors/abi/sector.go
    pub static ref WINDOW_POST_SECTOR_COUNT: RwLock<HashMap<u64, usize>> = RwLock::new(
        [
            (SECTOR_SIZE_2_KIB, 2),
            (SECTOR_SIZE_4_KIB, 2),
            (SECTOR_SIZE_16_KIB, 2),
            (SECTOR_SIZE_32_KIB, 2),
            (SECTOR_SIZE_8_MIB, 2),
            (SECTOR_SIZE_16_MIB, 2),
            (SECTOR_SIZE_512_MIB, 2),
            (SECTOR_SIZE_1_GIB, 25), // performance tests revealed that only ~25 partitions can be verified on-chain using WASM with Polkadot's 6sec block times.
            (SECTOR_SIZE_32_GIB, 2349), // this gives 125,279,217 constraints, fitting in a single partition
            (SECTOR_SIZE_64_GIB, 2300), // this gives 129,887,900 constraints, fitting in a single partition
        ]
        .iter()
        .copied()
        .collect()
    );
}

/// Returns the minimum number of challenges used for the (synth and non-synth) interactive PoRep
/// for a certain sector size.
pub(crate) const fn get_porep_interactive_minimum_challenges(sector_size: u64) -> usize {
    match sector_size {
        SECTOR_SIZE_2_KIB | SECTOR_SIZE_4_KIB | SECTOR_SIZE_16_KIB | SECTOR_SIZE_32_KIB
        | SECTOR_SIZE_8_MIB | SECTOR_SIZE_16_MIB | SECTOR_SIZE_512_MIB => 2,
        SECTOR_SIZE_1_GIB | SECTOR_SIZE_32_GIB | SECTOR_SIZE_64_GIB => 176,
        _ => panic!("invalid sector size"),
    }
}

/// Returns the minimum number of challenges used for the non-interactive PoRep fo a certain sector
/// size, i.e. `ceil(12.8 * interactive_porep_min_challenges)`.
pub(crate) const fn get_porep_non_interactive_minimum_challenges(sector_size: u64) -> usize {
    match sector_size {
        SECTOR_SIZE_2_KIB | SECTOR_SIZE_4_KIB | SECTOR_SIZE_16_KIB | SECTOR_SIZE_32_KIB
        | SECTOR_SIZE_8_MIB | SECTOR_SIZE_16_MIB | SECTOR_SIZE_512_MIB | SECTOR_SIZE_1_GIB => 26,
        SECTOR_SIZE_32_GIB | SECTOR_SIZE_64_GIB => 2253,
        _ => panic!("invalid sector size"),
    }
}

/// Returns the number of partitions for non-interactive PoRep for a certain sector size.
pub const fn get_porep_non_interactive_partitions(sector_size: u64) -> u8 {
    match sector_size {
        // The filename of the parameter files and verifying keys depend on the number of
        // challenges per partition. In order to be able to re-use the files that were generated
        // for the interactive PoRep, we need to use certain numbers, also for the test sector
        // sizes. The number of challenges per partition for test sizes is 2, for production
        // parameters it's 18.
        SECTOR_SIZE_2_KIB | SECTOR_SIZE_4_KIB | SECTOR_SIZE_16_KIB | SECTOR_SIZE_32_KIB
        | SECTOR_SIZE_8_MIB | SECTOR_SIZE_16_MIB | SECTOR_SIZE_512_MIB | SECTOR_SIZE_1_GIB => 13,
        SECTOR_SIZE_32_GIB | SECTOR_SIZE_64_GIB => 126,
        _ => panic!("invalid sector size"),
    }
}

/// The size of a single snark proof.
pub const SINGLE_PARTITION_PROOF_LEN: usize = 192;

pub const MINIMUM_RESERVED_LEAVES_FOR_PIECE_IN_SECTOR: u64 = 4;

// Bit padding causes bytes to only be aligned at every 127 bytes (for 31.75 bytes).
pub const MINIMUM_RESERVED_BYTES_FOR_PIECE_IN_FULLY_ALIGNED_SECTOR: u64 =
    (MINIMUM_RESERVED_LEAVES_FOR_PIECE_IN_SECTOR * NODE_SIZE as u64) - 1;

/// The minimum size a single piece must have before padding.
pub const MIN_PIECE_SIZE: UnpaddedBytesAmount = UnpaddedBytesAmount(127);

/// The maximum number of challenges per partition the circuits can work with.
pub(crate) const MAX_CHALLENGES_PER_PARTITION: u8 = 18;

/// The hasher used for creating comm_d.
pub type DefaultPieceHasher = Sha256Hasher;
pub type DefaultPieceDomain = <DefaultPieceHasher as Hasher>::Domain;

/// The default hasher for merkle trees currently in use.
pub type DefaultTreeHasher = PoseidonHasher;
pub type DefaultTreeDomain = <DefaultTreeHasher as Hasher>::Domain;

/// A binary merkle tree with Poseidon hashing, where all levels have arity 2. It's fully
/// persisted to disk.
pub type DefaultBinaryTree = BinaryMerkleTree<DefaultTreeHasher>;
/// A merkle tree with Poseidon hashing, where all levels have arity 8. It's fully persisted to
/// disk.
pub type DefaultOctTree = DiskTree<DefaultTreeHasher, U8, U0, U0>;
/// A merkle tree with Poseidon hashing, where all levels have arity 8. Some levels are not
/// persisted to disk, but only cached in memory.
pub type DefaultOctLCTree = LCTree<DefaultTreeHasher, U8, U0, U0>;

// Generic shapes
pub type SectorShapeBase = LCTree<DefaultTreeHasher, U8, U0, U0>;
pub type SectorShapeSub2 = LCTree<DefaultTreeHasher, U8, U2, U0>;
pub type SectorShapeSub8 = LCTree<DefaultTreeHasher, U8, U8, U0>;
pub type SectorShapeTop2 = LCTree<DefaultTreeHasher, U8, U8, U2>;

// Specific size constants by shape
pub type SectorShape2KiB = SectorShapeBase;
pub type SectorShape8MiB = SectorShapeBase;
pub type SectorShape512MiB = SectorShapeBase;

pub type SectorShape4KiB = SectorShapeSub2;
pub type SectorShape16MiB = SectorShapeSub2;
pub type SectorShape1GiB = SectorShapeSub2;

pub type SectorShape16KiB = SectorShapeSub8;
pub type SectorShape32GiB = SectorShapeSub8;

pub type SectorShape32KiB = SectorShapeTop2;
pub type SectorShape64GiB = SectorShapeTop2;

pub fn is_sector_shape_base(sector_size: u64) -> bool {
    matches!(
        sector_size,
        SECTOR_SIZE_2_KIB | SECTOR_SIZE_8_MIB | SECTOR_SIZE_512_MIB
    )
}

pub fn is_sector_shape_sub2(sector_size: u64) -> bool {
    matches!(
        sector_size,
        SECTOR_SIZE_4_KIB | SECTOR_SIZE_16_MIB | SECTOR_SIZE_1_GIB
    )
}

pub fn is_sector_shape_sub8(sector_size: u64) -> bool {
    matches!(sector_size, SECTOR_SIZE_16_KIB | SECTOR_SIZE_32_GIB)
}

pub fn is_sector_shape_top2(sector_size: u64) -> bool {
    matches!(sector_size, SECTOR_SIZE_32_KIB | SECTOR_SIZE_64_GIB)
}

/// Calls a function with the type hint of the sector shape matching the provided sector.
/// Panics if provided with an unknown sector size.
#[macro_export]
macro_rules! with_shape {
    ($size:expr, $f:ident) => {
        with_shape!($size, $f,)
    };
    ($size:expr, $f:ident, $($args:expr,)*) => {
        match $size {
            _x if $size == $crate::constants::SECTOR_SIZE_2_KIB => {
              $f::<$crate::constants::SectorShape2KiB>($($args),*)
            },
            _x if $size == $crate::constants::SECTOR_SIZE_4_KIB => {
              $f::<$crate::constants::SectorShape4KiB>($($args),*)
            },
            _x if $size == $crate::constants::SECTOR_SIZE_16_KIB => {
              $f::<$crate::constants::SectorShape16KiB>($($args),*)
            },
            _x if $size == $crate::constants::SECTOR_SIZE_32_KIB => {
              $f::<$crate::constants::SectorShape32KiB>($($args),*)
            },
            _xx if $size == $crate::constants::SECTOR_SIZE_8_MIB => {
              $f::<$crate::constants::SectorShape8MiB>($($args),*)
            },
            _xx if $size == $crate::constants::SECTOR_SIZE_16_MIB => {
              $f::<$crate::constants::SectorShape16MiB>($($args),*)
            },
            _x if $size == $crate::constants::SECTOR_SIZE_512_MIB => {
              $f::<$crate::constants::SectorShape512MiB>($($args),*)
            },
            _x if $size == $crate::constants::SECTOR_SIZE_1_GIB => {
              $f::<$crate::constants::SectorShape1GiB>($($args),*)
            },
            _x if $size == $crate::constants::SECTOR_SIZE_32_GIB => {
              $f::<$crate::constants::SectorShape32GiB>($($args),*)
            },
            _x if $size == $crate::constants::SECTOR_SIZE_64_GIB => {
              $f::<$crate::constants::SectorShape64GiB>($($args),*)
            },
            _ => panic!("unsupported sector size: {}", $size),
        }
    };
    ($size:expr, $f:ident, $($args:expr),*) => {
        with_shape!($size, $f, $($args,)*)
    };
}

pub const TEST_SEED: [u8; 16] = [
    0x59, 0x62, 0xbe, 0x5d, 0x76, 0x3d, 0x31, 0x8d, 0x17, 0xdb, 0x37, 0x32, 0x54, 0x06, 0xbc, 0xe5,
];
