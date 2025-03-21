use std::fs::{File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{ensure, Context, Result};
use filecoin_hashers::Hasher;
use fr32::{write_unpadded, Fr32Reader};
use log::{info, trace};
use memmap2::MmapOptions;
use merkletree::store::{DiskStore, LevelCacheStore, StoreConfig};
use storage_proofs_core::{
    cache_key::CacheKey,
    measurements::{measure_op, Operation},
    merkle::get_base_tree_count,
    pieces::generate_piece_commitment_bytes_from_source,
    sector::SectorId,
};
use storage_proofs_porep::stacked::{self, generate_replica_id, PublicParams, StackedDrg};
use typenum::Unsigned;

use crate::{
    commitment_reader::CommitmentReader,
    constants::{
        DefaultBinaryTree, DefaultOctTree, DefaultPieceDomain, DefaultPieceHasher,
        MINIMUM_RESERVED_BYTES_FOR_PIECE_IN_FULLY_ALIGNED_SECTOR as MINIMUM_PIECE_SIZE,
    },
    parameters::public_params,
    pieces::{get_piece_alignment, sum_piece_bytes_with_alignment},
    types::{
        Commitment, MerkleTreeTrait, PaddedBytesAmount, PieceInfo, PoRepConfig, ProverId,
        SealPreCommitPhase1Output, Ticket, UnpaddedByteIndex, UnpaddedBytesAmount,
    },
};

mod fake_seal;
mod post_util;
mod seal;
mod update;
mod util;
mod window_post;
mod winning_post;

pub use fake_seal::*;
pub use post_util::*;
pub use seal::*;
pub use update::*;
pub use util::*;
pub use window_post::*;
pub use winning_post::*;

pub use storage_proofs_update::constants::{partition_count, TreeRHasher};

pub fn clear_cache(cache_dir: &Path) -> Result<()> {
    info!("clear_cache:start");

    let result = stacked::clear_cache_dir(cache_dir);

    info!("clear_cache:finish");

    result
}

pub fn clear_synthetic_proofs(cache_dir: &Path) -> Result<()> {
    info!("clear_synthetic_proofs:start");

    let result = stacked::clear_synthetic_proofs(cache_dir);

    info!("clear_synthetic_proofs:finish");

    result
}

/// Unseals the sector at `sealed_path` and returns the bytes for a piece
/// whose first (unpadded) byte begins at `offset` and ends at `offset` plus
/// `num_bytes`, inclusive. Note that the entire sector is unsealed each time
/// this function is called.
///
/// # Arguments
///
/// * `porep_config` - porep configuration containing the sector size.
/// * `cache_path` - path to the directory in which the sector data's Merkle Tree is written.
/// * `sealed_path` - path to the sealed sector file that we will unseal and read a byte range.
/// * `output_path` - path to a file that we will write the requested byte range to.
/// * `prover_id` - the prover-id that sealed the sector.
/// * `sector_id` - the sector-id of the sealed sector.
/// * `comm_d` - the commitment to the sector's data.
/// * `ticket` - the ticket that was used to generate the sector's replica-id.
/// * `offset` - the byte index in the unsealed sector of the first byte that we want to read.
/// * `num_bytes` - the number of bytes that we want to read.
#[allow(clippy::too_many_arguments)]
pub fn get_unsealed_range<T: Into<PathBuf> + AsRef<Path>, Tree: 'static + MerkleTreeTrait>(
    porep_config: &PoRepConfig,
    cache_path: T,
    sealed_path: T,
    output_path: T,
    prover_id: ProverId,
    sector_id: SectorId,
    comm_d: Commitment,
    ticket: Ticket,
    offset: UnpaddedByteIndex,
    num_bytes: UnpaddedBytesAmount,
) -> Result<UnpaddedBytesAmount> {
    info!("get_unsealed_range:start");

    let f_out = File::create(&output_path)
        .with_context(|| format!("could not create output_path={:?}", output_path.as_ref()))?;

    let buf_f_out = BufWriter::new(f_out);

    let result = unseal_range_mapped::<_, _, Tree>(
        porep_config,
        cache_path,
        sealed_path.into(),
        buf_f_out,
        prover_id,
        sector_id,
        comm_d,
        ticket,
        offset,
        num_bytes,
    );

    info!("get_unsealed_range:finish");
    result
}

/// Unseals the sector read from `sealed_sector` and returns the bytes for a
/// piece whose first (unpadded) byte begins at `offset` and ends at `offset`
/// plus `num_bytes`, inclusive. Note that the entire sector is unsealed each
/// time this function is called.
///
/// # Arguments
///
/// * `porep_config` - porep configuration containing the sector size.
/// * `cache_path` - path to the directory in which the sector data's Merkle Tree is written.
/// * `sealed_sector` - a byte source from which we read sealed sector data.
/// * `unsealed_output` - a byte sink to which we write unsealed, un-bit-padded sector bytes.
/// * `prover_id` - the prover-id that sealed the sector.
/// * `sector_id` - the sector-id of the sealed sector.
/// * `comm_d` - the commitment to the sector's data.
/// * `ticket` - the ticket that was used to generate the sector's replica-id.
/// * `offset` - the byte index in the unsealed sector of the first byte that we want to read.
/// * `num_bytes` - the number of bytes that we want to read.
#[allow(clippy::too_many_arguments)]
pub fn unseal_range<P, R, W, Tree>(
    porep_config: &PoRepConfig,
    cache_path: P,
    mut sealed_sector: R,
    unsealed_output: W,
    prover_id: ProverId,
    sector_id: SectorId,
    comm_d: Commitment,
    ticket: Ticket,
    offset: UnpaddedByteIndex,
    num_bytes: UnpaddedBytesAmount,
) -> Result<UnpaddedBytesAmount>
where
    P: Into<PathBuf> + AsRef<Path>,
    R: Read,
    W: Write,
    Tree: 'static + MerkleTreeTrait,
{
    info!("unseal_range:start");
    ensure!(comm_d != [0; 32], "Invalid all zero commitment (comm_d)");

    let comm_d =
        as_safe_commitment::<<DefaultPieceHasher as Hasher>::Domain, _>(&comm_d, "comm_d")?;

    let replica_id = generate_replica_id::<Tree::Hasher, _>(
        &prover_id,
        sector_id.into(),
        &ticket,
        comm_d,
        &porep_config.porep_id,
    );

    let mut data = Vec::new();
    sealed_sector.read_to_end(&mut data)?;

    let res = unseal_range_inner::<_, _, Tree>(
        porep_config,
        cache_path,
        &mut data,
        unsealed_output,
        replica_id,
        offset,
        num_bytes,
    )?;

    info!("unseal_range:finish");

    Ok(res)
}

/// Unseals the sector read from `sealed_sector` and returns the bytes for a
/// piece whose first (unpadded) byte begins at `offset` and ends at `offset`
/// plus `num_bytes`, inclusive. Note that the entire sector is unsealed each
/// time this function is called.
///
/// # Arguments
///
/// * `porep_config` - porep configuration containing the sector size.
/// * `cache_path` - path to the directory in which the sector data's Merkle Tree is written.
/// * `sealed_sector` - a byte source from which we read sealed sector data.
/// * `unsealed_output` - a byte sink to which we write unsealed, un-bit-padded sector bytes.
/// * `prover_id` - the prover-id that sealed the sector.
/// * `sector_id` - the sector-id of the sealed sector.
/// * `comm_d` - the commitment to the sector's data.
/// * `ticket` - the ticket that was used to generate the sector's replica-id.
/// * `offset` - the byte index in the unsealed sector of the first byte that we want to read.
/// * `num_bytes` - the number of bytes that we want to read.
#[allow(clippy::too_many_arguments)]
pub fn unseal_range_mapped<P, W, Tree>(
    porep_config: &PoRepConfig,
    cache_path: P,
    sealed_path: PathBuf,
    unsealed_output: W,
    prover_id: ProverId,
    sector_id: SectorId,
    comm_d: Commitment,
    ticket: Ticket,
    offset: UnpaddedByteIndex,
    num_bytes: UnpaddedBytesAmount,
) -> Result<UnpaddedBytesAmount>
where
    P: Into<PathBuf> + AsRef<Path>,
    W: Write,
    Tree: 'static + MerkleTreeTrait,
{
    info!("unseal_range_mapped:start");
    ensure!(comm_d != [0; 32], "Invalid all zero commitment (comm_d)");

    let comm_d =
        as_safe_commitment::<<DefaultPieceHasher as Hasher>::Domain, _>(&comm_d, "comm_d")?;

    let replica_id = generate_replica_id::<Tree::Hasher, _>(
        &prover_id,
        sector_id.into(),
        &ticket,
        comm_d,
        &porep_config.porep_id,
    );

    let mapped_file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(sealed_path)?;
    let mut data = unsafe { MmapOptions::new().map_copy(&mapped_file)? };

    let result = unseal_range_inner::<_, _, Tree>(
        porep_config,
        cache_path,
        &mut data,
        unsealed_output,
        replica_id,
        offset,
        num_bytes,
    );
    info!("unseal_range_mapped:finish");

    result
}

/// Unseals the sector read from `sealed_sector` and returns the bytes for a
/// piece whose first (unpadded) byte begins at `offset` and ends at `offset`
/// plus `num_bytes`, inclusive. Note that the entire sector is unsealed each
/// time this function is called.
///
/// # Arguments
///
/// * `porep_config` - porep configuration containing the sector size.
/// * `cache_path` - path to the directory in which the sector data's Merkle Tree is written.
/// * `sealed_sector` - a byte source from which we read sealed sector data.
/// * `unsealed_output` - a byte sink to which we write unsealed, un-bit-padded sector bytes.
/// * `prover_id` - the prover-id that sealed the sector.
/// * `sector_id` - the sector-id of the sealed sector.
/// * `comm_d` - the commitment to the sector's data.
/// * `ticket` - the ticket that was used to generate the sector's replica-id.
/// * `offset` - the byte index in the unsealed sector of the first byte that we want to read.
/// * `num_bytes` - the number of bytes that we want to read.
#[allow(clippy::too_many_arguments)]
fn unseal_range_inner<P, W, Tree>(
    porep_config: &PoRepConfig,
    cache_path: P,
    data: &mut [u8],
    mut unsealed_output: W,
    replica_id: <Tree::Hasher as Hasher>::Domain,
    offset: UnpaddedByteIndex,
    num_bytes: UnpaddedBytesAmount,
) -> Result<UnpaddedBytesAmount>
where
    P: Into<PathBuf> + AsRef<Path>,
    W: Write,
    Tree: 'static + MerkleTreeTrait,
{
    trace!("unseal_range_inner:start");

    let config = StoreConfig::new(cache_path.as_ref(), CacheKey::CommDTree.to_string(), 0);
    let pp: PublicParams<Tree> = public_params(porep_config)?;

    let offset_padded: PaddedBytesAmount = UnpaddedBytesAmount::from(offset).into();
    let num_bytes_padded: PaddedBytesAmount = num_bytes.into();

    StackedDrg::<Tree, DefaultPieceHasher>::extract_and_invert_transform_layers(
        &pp.graph,
        pp.num_layers,
        &replica_id,
        data,
        config,
    )?;
    let start: usize = offset_padded.into();
    let end = start + usize::from(num_bytes_padded);
    let unsealed = &data[start..end];

    // If the call to `extract_range` was successful, the `unsealed` vector must
    // have a length which equals `num_bytes_padded`. The byte at its 0-index
    // byte will be the byte at index `offset_padded` in the sealed sector.
    let written = write_unpadded(unsealed, &mut unsealed_output, 0, num_bytes.into())
        .context("write_unpadded failed")?;

    let amount = UnpaddedBytesAmount(written as u64);

    trace!("unseal_range_inner:finish");
    Ok(amount)
}

/// Generates a piece commitment for the provided byte source. Returns an error
/// if the byte source produced more than `piece_size` bytes.
///
/// # Arguments
///
/// * `source` - a readable source of unprocessed piece bytes. The piece's commitment will be
///    generated for the bytes read from the source plus any added padding.
/// * `piece_size` - the number of unpadded user-bytes which can be read from source before EOF.
pub fn generate_piece_commitment<T: Read>(
    source: T,
    piece_size: UnpaddedBytesAmount,
) -> Result<PieceInfo> {
    trace!("generate_piece_commitment:start");

    let result = measure_op(Operation::GeneratePieceCommitment, || {
        ensure_piece_size(piece_size)?;

        // send the source through the preprocessor
        let source = BufReader::new(source);
        let mut fr32_reader = Fr32Reader::new(source);

        let commitment = generate_piece_commitment_bytes_from_source::<DefaultPieceHasher>(
            &mut fr32_reader,
            PaddedBytesAmount::from(piece_size).into(),
        )?;

        PieceInfo::new(commitment, piece_size)
    });

    trace!("generate_piece_commitment:finish");
    result
}

/// Computes a NUL-byte prefix and/or suffix for `source` using the provided
/// `piece_lengths` and `piece_size` (such that the `source`, after
/// preprocessing, will occupy a subtree of a merkle tree built using the bytes
/// from `target`), runs the resultant byte stream through the preprocessor,
/// and writes the result to `target`. Returns a tuple containing the number of
/// bytes written to `target` (`source` plus alignment) and the commitment.
///
/// WARNING: Depending on the ordering and size of the pieces in
/// `piece_lengths`, this function could write a prefix of NUL bytes which
/// wastes ($SIZESECTORSIZE/2)-$MINIMUM_PIECE_SIZE space. This function will be
/// deprecated in favor of `write_and_preprocess`, and miners will be prevented
/// from sealing sectors containing more than $TOOMUCH alignment bytes.
///
/// # Arguments
///
/// * `source` - a readable source of unprocessed piece bytes.
/// * `target` - a writer where we will write the processed piece bytes.
/// * `piece_size` - the number of unpadded user-bytes which can be read from source before EOF.
/// * `piece_lengths` - the number of bytes for each previous piece in the sector.
pub fn add_piece<R, W>(
    source: R,
    target: W,
    piece_size: UnpaddedBytesAmount,
    piece_lengths: &[UnpaddedBytesAmount],
) -> Result<(PieceInfo, UnpaddedBytesAmount)>
where
    R: Read,
    W: Write,
{
    trace!("add_piece:start");

    let result = measure_op(Operation::AddPiece, || {
        ensure_piece_size(piece_size)?;

        let source = BufReader::new(source);
        let mut target = BufWriter::new(target);

        let written_bytes = sum_piece_bytes_with_alignment(piece_lengths);
        let piece_alignment = get_piece_alignment(written_bytes, piece_size);
        let fr32_reader = Fr32Reader::new(source);

        // write left alignment
        for _ in 0..usize::from(PaddedBytesAmount::from(piece_alignment.left_bytes)) {
            target.write_all(&[0u8][..])?;
        }

        let mut commitment_reader = CommitmentReader::new(fr32_reader);
        let n = io::copy(&mut commitment_reader, &mut target)
            .context("failed to write and preprocess bytes")?;

        ensure!(n != 0, "add_piece: read 0 bytes before EOF from source");
        let n = PaddedBytesAmount(n);
        let n: UnpaddedBytesAmount = n.into();

        ensure!(n == piece_size, "add_piece: invalid bytes amount written");

        // write right alignment
        for _ in 0..usize::from(PaddedBytesAmount::from(piece_alignment.right_bytes)) {
            target.write_all(&[0u8][..])?;
        }

        let commitment = commitment_reader.finish()?;
        let mut comm = [0u8; 32];
        comm.copy_from_slice(commitment.as_ref());

        let written = piece_alignment.left_bytes + piece_alignment.right_bytes + piece_size;

        Ok((PieceInfo::new(comm, n)?, written))
    });

    trace!("add_piece:finish");
    result
}

fn ensure_piece_size(piece_size: UnpaddedBytesAmount) -> Result<()> {
    ensure!(
        piece_size >= UnpaddedBytesAmount(MINIMUM_PIECE_SIZE),
        "Piece must be at least {} bytes",
        MINIMUM_PIECE_SIZE
    );

    let padded_piece_size: PaddedBytesAmount = piece_size.into();
    ensure!(
        u64::from(padded_piece_size).is_power_of_two(),
        "Bit-padded piece size must be a power of 2 ({:?})",
        padded_piece_size,
    );

    Ok(())
}

/// Writes bytes from `source` to `target`, adding bit-padding ("preprocessing")
/// as needed. Returns a tuple containing the number of bytes written to
/// `target` and the commitment.
///
/// WARNING: This function neither prepends nor appends alignment bytes to the
/// `target`; it is the caller's responsibility to ensure properly sized
/// and ordered writes to `target` such that `source`-bytes occupy whole
/// subtrees of the final merkle tree built over `target`.
///
/// # Arguments
///
/// * `source` - a readable source of unprocessed piece bytes.
/// * `target` - a writer where we will write the processed piece bytes.
/// * `piece_size` - the number of unpadded user-bytes which can be read from source before EOF.
pub fn write_and_preprocess<R, W>(
    source: R,
    target: W,
    piece_size: UnpaddedBytesAmount,
) -> Result<(PieceInfo, UnpaddedBytesAmount)>
where
    R: Read,
    W: Write,
{
    add_piece(source, target, piece_size, Default::default())
}

// Verifies if a DiskStore specified by a config (or set of 'required_configs' is consistent).
fn verify_store(config: &StoreConfig, arity: usize, required_configs: usize) -> Result<()> {
    let store_path = StoreConfig::data_path(&config.path, &config.id);
    if !Path::new(&store_path).exists() {
        // Configs may have split due to sector size, so we need to
        // check deterministic paths from here.
        let orig_path = store_path
            .clone()
            .into_os_string()
            .into_string()
            .expect("failed to convert store_path to string");
        let mut configs: Vec<StoreConfig> = Vec::with_capacity(required_configs);
        for i in 0..required_configs {
            let cur_path = orig_path
                .clone()
                .replace(".dat", format!("-{}.dat", i).as_str());

            if Path::new(&cur_path).exists() {
                let path_str = cur_path.as_str();
                let tree_names = vec!["tree-d", "tree-c", "tree-r-last"];
                for name in tree_names {
                    if path_str.contains(name) {
                        configs.push(StoreConfig::from_config(
                            config,
                            format!("{}-{}", name, i),
                            None,
                        ));
                        break;
                    }
                }
            }
        }

        ensure!(
            configs.len() == required_configs,
            "Missing store file (or associated split paths): {}",
            store_path.display()
        );

        let store_len = config.size.expect("disk store size not configured");
        for config in &configs {
            let data_path = StoreConfig::data_path(&config.path, &config.id);
            trace!(
                "verify_store: {:?} has length {} bytes",
                &data_path,
                std::fs::metadata(&data_path)?.len()
            );
            ensure!(
                DiskStore::<DefaultPieceDomain>::is_consistent(store_len, arity, config,)?,
                "Store is inconsistent: {:?}",
                &data_path
            );
        }
    } else {
        trace!(
            "verify_store: {:?} has length {}",
            &store_path,
            std::fs::metadata(&store_path)?.len()
        );
        ensure!(
            DiskStore::<DefaultPieceDomain>::is_consistent(
                config.size.expect("disk store size not configured"),
                arity,
                config,
            )?,
            "Store is inconsistent: {:?}",
            store_path
        );
    }

    Ok(())
}

// Verifies if a LevelCacheStore specified by a config is consistent.
fn verify_level_cache_store<Tree: MerkleTreeTrait>(config: &StoreConfig) -> Result<()> {
    let store_path = StoreConfig::data_path(&config.path, &config.id);
    if !Path::new(&store_path).exists() {
        let required_configs = get_base_tree_count::<Tree>();

        // Configs may have split due to sector size, so we need to
        // check deterministic paths from here.
        let orig_path = store_path
            .clone()
            .into_os_string()
            .into_string()
            .expect("failed to convert store_path to string");
        let mut configs: Vec<StoreConfig> = Vec::with_capacity(required_configs);
        for i in 0..required_configs {
            let cur_path = orig_path
                .clone()
                .replace(".dat", format!("-{}.dat", i).as_str());

            if Path::new(&cur_path).exists() {
                let path_str = cur_path.as_str();
                let tree_names = vec!["tree-d", "tree-c", "tree-r-last"];
                for name in tree_names {
                    if path_str.contains(name) {
                        configs.push(StoreConfig::from_config(
                            config,
                            format!("{}-{}", name, i),
                            None,
                        ));
                        break;
                    }
                }
            }
        }

        ensure!(
            configs.len() == required_configs,
            "Missing store file (or associated split paths): {}",
            store_path.display()
        );

        let store_len = config.size.expect("disk store size not configured");
        for config in &configs {
            let data_path = StoreConfig::data_path(&config.path, &config.id);
            trace!(
                "verify_store: {:?} has length {}",
                &data_path,
                std::fs::metadata(&data_path)?.len()
            );
            ensure!(
                LevelCacheStore::<DefaultPieceDomain, File>::is_consistent(
                    store_len,
                    Tree::Arity::to_usize(),
                    config,
                )?,
                "Store is inconsistent: {:?}",
                &data_path
            );
        }
    } else {
        trace!(
            "verify_store: {:?} has length {}",
            &store_path,
            std::fs::metadata(&store_path)?.len()
        );
        ensure!(
            LevelCacheStore::<DefaultPieceDomain, File>::is_consistent(
                config.size.expect("disk store size not configured"),
                Tree::Arity::to_usize(),
                config,
            )?,
            "Store is inconsistent: {:?}",
            store_path
        );
    }

    Ok(())
}

// Checks for the existence of the tree d store, the replica, and all generated labels.
pub fn validate_cache_for_precommit_phase2<R, T, Tree: MerkleTreeTrait>(
    cache_path: R,
    replica_path: T,
    seal_precommit_phase1_output: &SealPreCommitPhase1Output<Tree>,
) -> Result<()>
where
    R: AsRef<Path>,
    T: AsRef<Path>,
{
    info!("validate_cache_for_precommit_phase2:start");

    ensure!(
        replica_path.as_ref().exists(),
        "Missing replica: {}",
        replica_path.as_ref().to_path_buf().display()
    );

    // Verify all stores/labels within the Labels object, but
    // respecting the current cache_path.
    let cache = cache_path.as_ref().to_path_buf();
    seal_precommit_phase1_output
        .labels
        .verify_stores(verify_store, &cache)?;

    // Update the previous phase store path to the current cache_path.
    let mut config = StoreConfig::from_config(
        &seal_precommit_phase1_output.config,
        &seal_precommit_phase1_output.config.id,
        seal_precommit_phase1_output.config.size,
    );
    config.path = cache_path.as_ref().into();

    let result = verify_store(
        &config,
        <DefaultBinaryTree as MerkleTreeTrait>::Arity::to_usize(),
        get_base_tree_count::<Tree>(),
    );

    info!("validate_cache_for_precommit_phase2:finish");
    result
}

// Checks for the existence of the replica data and t_aux, which in
// turn allows us to verify the tree d, tree r, tree c, and the
// labels.
pub fn validate_cache_for_commit<R, T, Tree: MerkleTreeTrait>(
    cache_path: R,
    replica_path: T,
) -> Result<()>
where
    R: AsRef<Path>,
    T: AsRef<Path>,
{
    info!("validate_cache_for_commit:start");

    // Verify that the replica exists and is not empty.
    ensure!(
        replica_path.as_ref().exists(),
        "Missing replica: {}",
        replica_path.as_ref().to_path_buf().display()
    );

    let metadata = File::open(&replica_path)?.metadata()?;
    ensure!(
        metadata.len() > 0,
        "Replica {} exists, but is empty!",
        replica_path.as_ref().to_path_buf().display()
    );

    let cache = &cache_path.as_ref();

    // Make sure p_aux exists and is valid.
    let _ = util::get_p_aux::<Tree>(cache)?;

    let t_aux = util::get_t_aux::<Tree>(cache, metadata.len())?;

    // Verify all stores/labels within the Labels object.
    let cache = cache_path.as_ref().to_path_buf();
    t_aux.labels.verify_stores(verify_store, &cache)?;

    // Verify each tree disk store.
    verify_store(
        &t_aux.tree_d_config,
        <DefaultBinaryTree as MerkleTreeTrait>::Arity::to_usize(),
        get_base_tree_count::<Tree>(),
    )?;
    verify_store(
        &t_aux.tree_c_config,
        <DefaultOctTree as MerkleTreeTrait>::Arity::to_usize(),
        get_base_tree_count::<Tree>(),
    )?;
    verify_level_cache_store::<DefaultOctTree>(&t_aux.tree_r_last_config)?;

    info!("validate_cache_for_commit:finish");

    Ok(())
}
