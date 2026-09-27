//! A binary Merkle tree over chunk digests.
//!
//! The manifest already lists every chunk id, so the root is not needed to
//! verify a complete download. It is there so that a node can prove "this
//! chunk belongs to that video" without shipping the whole chunk list — the
//! shape partial and streaming transfers will want later.

use sha2::{Digest, Sha256};

use ovn_protocol::ContentId;

// Domain separation between leaves and interior nodes, so that an interior
// hash can never be passed off as a leaf (the classic second-preimage attack
// on naive Merkle trees).
const LEAF_PREFIX: u8 = 0x00;
const NODE_PREFIX: u8 = 0x01;

fn hash_leaf(digest: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update([LEAF_PREFIX]);
    hasher.update(digest);
    hasher.finalize().into()
}

fn hash_node(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update([NODE_PREFIX]);
    hasher.update(left);
    hasher.update(right);
    hasher.finalize().into()
}

fn leaves(chunks: &[ContentId]) -> Vec<[u8; 32]> {
    chunks.iter().map(|c| hash_leaf(c.digest())).collect()
}

/// Merkle root over a chunk list. An empty list hashes to all zeroes.
pub fn merkle_root(chunks: &[ContentId]) -> [u8; 32] {
    let mut level = leaves(chunks);
    if level.is_empty() {
        return [0u8; 32];
    }
    while level.len() > 1 {
        let mut next = Vec::with_capacity(level.len().div_ceil(2));
        for pair in level.chunks(2) {
            next.push(match pair {
                [left, right] => hash_node(left, right),
                // Odd node out is promoted by hashing it with itself, which
                // keeps every interior node domain-separated from a leaf.
                [left] => hash_node(left, left),
                _ => unreachable!("chunks(2) yields one or two elements"),
            });
        }
        level = next;
    }
    level[0]
}

/// Sibling hashes proving `index` is part of the tree, bottom level first.
/// Each entry is `(hash, sibling_is_on_the_right)`.
pub fn merkle_proof(chunks: &[ContentId], index: usize) -> Option<Vec<([u8; 32], bool)>> {
    if index >= chunks.len() {
        return None;
    }
    let mut level = leaves(chunks);
    let mut position = index;
    let mut proof = Vec::new();
    while level.len() > 1 {
        let sibling_on_right = position % 2 == 0;
        let sibling = if sibling_on_right {
            level.get(position + 1).copied().unwrap_or(level[position])
        } else {
            level[position - 1]
        };
        proof.push((sibling, sibling_on_right));
        let mut next = Vec::with_capacity(level.len().div_ceil(2));
        for pair in level.chunks(2) {
            next.push(match pair {
                [left, right] => hash_node(left, right),
                [left] => hash_node(left, left),
                _ => unreachable!(),
            });
        }
        level = next;
        position /= 2;
    }
    Some(proof)
}

/// Check a proof produced by [`merkle_proof`].
pub fn verify_merkle_proof(chunk: &ContentId, proof: &[([u8; 32], bool)], root: &[u8; 32]) -> bool {
    let mut current = hash_leaf(chunk.digest());
    for (sibling, sibling_on_right) in proof {
        current = if *sibling_on_right {
            hash_node(&current, sibling)
        } else {
            hash_node(sibling, &current)
        };
    }
    &current == root
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunks(n: usize) -> Vec<ContentId> {
        (0..n)
            .map(|i| ContentId::from_raw(format!("chunk {i}").as_bytes()))
            .collect()
    }

    #[test]
    fn empty_tree_has_a_zero_root() {
        assert_eq!(merkle_root(&[]), [0u8; 32]);
    }

    #[test]
    fn root_is_deterministic() {
        assert_eq!(merkle_root(&chunks(5)), merkle_root(&chunks(5)));
    }

    #[test]
    fn reordering_chunks_changes_the_root() {
        let ordered = chunks(4);
        let mut swapped = ordered.clone();
        swapped.swap(0, 3);
        assert_ne!(merkle_root(&ordered), merkle_root(&swapped));
    }

    #[test]
    fn changing_one_chunk_changes_the_root() {
        let mut modified = chunks(7);
        modified[3] = ContentId::from_raw(b"tampered");
        assert_ne!(merkle_root(&chunks(7)), merkle_root(&modified));
    }

    #[test]
    fn proofs_verify_for_every_index_at_odd_and_even_widths() {
        for width in 1..=9 {
            let cs = chunks(width);
            let root = merkle_root(&cs);
            for (i, chunk) in cs.iter().enumerate() {
                let proof = merkle_proof(&cs, i).expect("index in range");
                assert!(
                    verify_merkle_proof(chunk, &proof, &root),
                    "width {width} index {i}"
                );
            }
        }
    }

    #[test]
    fn a_proof_does_not_verify_for_a_foreign_chunk() {
        let cs = chunks(6);
        let root = merkle_root(&cs);
        let proof = merkle_proof(&cs, 2).unwrap();
        assert!(!verify_merkle_proof(
            &ContentId::from_raw(b"outsider"),
            &proof,
            &root
        ));
    }

    #[test]
    fn out_of_range_index_has_no_proof() {
        assert!(merkle_proof(&chunks(3), 3).is_none());
    }
}
