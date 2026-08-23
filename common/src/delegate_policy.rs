//! The delegate's decision functions.
//!
//! Pure: no I/O, no clock, no randomness. Everything the delegate decides is
//! decided here, so it can be tested on any platform — the delegate crate
//! itself cannot even be compiled on a Windows host.

use crate::delegate_api::{EntropyQuality, Refusal};
use sha2::{Digest, Sha256};

const DOMAIN_KEYGEN: &[u8] = b"freenet-chess-v1/keygen";

/// The result of probing the host RNG.
///
/// `freenet_stdlib::rand::rand_bytes` reads into a zero-initialised buffer via
/// a host import that is a no-op stub off-wasm, so it returns all zeros there
/// with no error. Treating that as entropy would mint a known private key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostEntropy {
    Live([u8; 32]),
    Dead,
}

/// Classify two independent draws from the host RNG.
///
/// All-zeros catches the off-wasm stub. Two identical draws catch a dead or
/// missing host source generally — a live CSPRNG repeats 32 bytes with
/// negligible probability.
pub fn classify_host_entropy(first: [u8; 32], second: [u8; 32]) -> HostEntropy {
    if first == [0u8; 32] || first == second {
        HostEntropy::Dead
    } else {
        HostEntropy::Live(first)
    }
}

/// Mix available entropy sources into a signing-key seed.
///
/// Mixing never loses: the result is at least as unpredictable as the
/// strongest input. Host entropy is the only source the UI does not control,
/// so it alone gives "the UI cannot learn the key at generation time"; caller
/// entropy still gives "the UI cannot learn it afterwards". With neither, this
/// fails closed rather than producing a guessable key.
pub fn derive_seed(
    host: HostEntropy,
    caller: Option<[u8; 32]>,
    label: &str,
) -> Result<([u8; 32], EntropyQuality), Refusal> {
    // A caller sending zeros is not contributing entropy, whatever it thinks.
    let caller = caller.filter(|c| c != &[0u8; 32]);

    let (host_bytes, quality) = match host {
        HostEntropy::Live(h) => (h, EntropyQuality::HostBacked),
        HostEntropy::Dead => {
            if caller.is_none() {
                return Err(Refusal::NoEntropy);
            }
            ([0u8; 32], EntropyQuality::Degraded)
        }
    };

    let mut h = Sha256::new();
    h.update(DOMAIN_KEYGEN);
    h.update(host_bytes);
    h.update(caller.unwrap_or([0u8; 32]));
    h.update((label.len() as u32).to_le_bytes());
    h.update(label.as_bytes());
    Ok((h.finalize().into(), quality))
}
