//! SSH Host Key Management.
//!
//! Manages the server's long-term identity keys:
//! - Ed25519 (classical, RFC 8709)
//! - ML-DSA-65 (post-quantum, FIPS 204)
//! - Hybrid: ML-DSA-65 + Ed25519 dual signature
//!
//! Provides SSH wire-format serialization for public keys and signatures,
//! and signing of the exchange hash during key exchange.

use alloc::vec::Vec;

use ed25519_dalek::{SigningKey as Ed25519SigningKey, VerifyingKey as Ed25519VerifyingKey};
use ed25519_dalek::{Signer as Ed25519Signer, Verifier as Ed25519Verifier};

use ml_dsa::{MlDsa65, VerifyingKey as MlDsaVerifyingKey};
use ml_dsa::{EncodedSignature as MlDsaEncodedSignature, EncodedVerifyingKey as MlDsaEncodedVerifyingKey};
use ml_dsa::signature::Signer as _;
use ml_dsa::signature::Verifier as _;
use ml_dsa::KeyGen;
use crate::wire::SshWriter;

// ---------------------------------------------------------------------------
// Host key type identifiers (SSH name strings)
// ---------------------------------------------------------------------------

/// SSH name for Ed25519 host keys (RFC 8709).
pub const SSH_ED25519: &str = "ssh-ed25519";

/// SSH name for hybrid ML-DSA-65 + Ed25519 host keys.
/// Following the naming convention from draft PQ SSH specs.
pub const SSH_MLDSA65_ED25519: &str = "mlkem768-ed25519@openssh.com";

// ---------------------------------------------------------------------------
// Ed25519 host key
// ---------------------------------------------------------------------------

/// An Ed25519 host keypair.
pub struct Ed25519HostKey {
    /// Ed25519 secret key seed (32 bytes).
    secret: [u8; 32],
    /// Ed25519 public key (32 bytes).
    public: [u8; 32],
}

impl Ed25519HostKey {
    /// Generate a new Ed25519 host keypair.
    pub fn generate(rng: &mut dyn FnMut(&mut [u8])) -> Self {
        log::info!("hostkey: generating Ed25519 host keypair");

        let mut secret = [0u8; 32];
        rng(&mut secret);

        // Real Ed25519 key derivation via ed25519-dalek
        let signing_key = Ed25519SigningKey::from_bytes(&secret);
        let public = signing_key.verifying_key().to_bytes();

        log::debug!(
            "hostkey: Ed25519 public key = {:02x}{:02x}{:02x}{:02x}...",
            public[0], public[1], public[2], public[3],
        );

        Self { secret, public }
    }

    /// Serialize the public key in SSH wire format.
    ///
    /// ```text
    /// string    "ssh-ed25519"
    /// string    public_key (32 bytes)
    /// ```
    pub fn public_key_blob(&self) -> Vec<u8> {
        let mut w = SshWriter::new();
        w.write_string_utf8(SSH_ED25519);
        w.write_string(&self.public);
        w.into_bytes()
    }

    /// Sign data and return the signature in SSH wire format.
    ///
    /// ```text
    /// string    "ssh-ed25519"
    /// string    signature (64 bytes)
    /// ```
    pub fn sign(&self, data: &[u8]) -> Vec<u8> {
        log::debug!("hostkey: signing {} bytes with Ed25519", data.len());

        // Real Ed25519 signing via ed25519-dalek
        let signing_key = Ed25519SigningKey::from_bytes(&self.secret);
        let sig = signing_key.sign(data);
        let sig_bytes = sig.to_bytes();

        let mut w = SshWriter::new();
        w.write_string_utf8(SSH_ED25519);
        w.write_string(&sig_bytes);
        w.into_bytes()
    }

    /// Verify an Ed25519 signature over data using a raw 32-byte public key.
    pub fn verify(public_key: &[u8; 32], data: &[u8], signature: &[u8]) -> bool {
        log::debug!(
            "hostkey: verifying Ed25519 signature — data={} bytes, sig={} bytes",
            data.len(),
            signature.len(),
        );

        // Real Ed25519 verification via ed25519-dalek
        let vk = match Ed25519VerifyingKey::from_bytes(public_key) {
            Ok(vk) => vk,
            Err(e) => {
                log::error!("hostkey: invalid Ed25519 public key: {}", e);
                return false;
            }
        };

        if signature.len() != 64 {
            log::error!("hostkey: Ed25519 signature wrong length: {} (expected 64)", signature.len());
            return false;
        }

        let mut sig_bytes = [0u8; 64];
        sig_bytes.copy_from_slice(signature);
        let sig = ed25519_dalek::Signature::from_bytes(&sig_bytes);

        match vk.verify(data, &sig) {
            Ok(()) => {
                log::debug!("hostkey: Ed25519 signature verified successfully");
                true
            }
            Err(e) => {
                log::debug!("hostkey: Ed25519 signature verification failed: {}", e);
                false
            }
        }
    }

    /// Get the raw 32-byte public key.
    pub fn public_key_bytes(&self) -> &[u8; 32] {
        &self.public
    }

    /// Serialize the full keypair for persistence (secret || public).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(64);
        out.extend_from_slice(&self.secret);
        out.extend_from_slice(&self.public);
        out
    }

    /// Deserialize a keypair from persistence.
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < 64 {
            log::error!("hostkey: Ed25519 key data too short: {} bytes", data.len());
            return None;
        }
        let mut secret = [0u8; 32];
        let mut public = [0u8; 32];
        secret.copy_from_slice(&data[..32]);
        public.copy_from_slice(&data[32..64]);

        // Validate that the public key matches the secret
        let signing_key = Ed25519SigningKey::from_bytes(&secret);
        let derived_public = signing_key.verifying_key().to_bytes();
        if derived_public != public {
            log::error!("hostkey: Ed25519 key mismatch — stored public key doesn't match derived");
            return None;
        }

        log::debug!("hostkey: Ed25519 keypair loaded from persistence");
        Some(Self { secret, public })
    }
}

// ---------------------------------------------------------------------------
// ML-DSA-65 host key (post-quantum)
// ---------------------------------------------------------------------------

/// An ML-DSA-65 host keypair (FIPS 204 / CRYSTALS-Dilithium).
///
/// Internally stores the 32-byte seed from which the signing key is derived
/// deterministically, plus the encoded verifying (public) key.
pub struct MlDsa65HostKey {
    /// ML-DSA-65 seed (32 bytes) — used to derive the full signing key.
    seed: [u8; 32],
    /// ML-DSA-65 encoded public/verifying key.
    public: Vec<u8>,
}

/// ML-DSA-65 public key size (FIPS 204).
pub const MLDSA65_PK_SIZE: usize = 1952;

/// ML-DSA-65 secret key size (FIPS 204).
pub const MLDSA65_SK_SIZE: usize = 4032;

/// ML-DSA-65 signature size (FIPS 204).
pub const MLDSA65_SIG_SIZE: usize = 3309;

impl MlDsa65HostKey {
    /// Generate a new ML-DSA-65 host keypair.
    pub fn generate(rng: &mut dyn FnMut(&mut [u8])) -> Self {
        log::info!("hostkey: generating ML-DSA-65 host keypair");

        // Generate a random 32-byte seed
        let mut seed = [0u8; 32];
        rng(&mut seed);

        // Derive the keypair deterministically from the seed via ml-dsa
        let seed_array = ml_dsa::B32::from(seed);
        let kp = MlDsa65::from_seed(&seed_array);

        // Encode the verifying (public) key
        use ml_dsa::signature::Keypair;
        let vk = kp.verifying_key();
        let public = Vec::from(vk.encode().as_slice());

        log::debug!(
            "hostkey: ML-DSA-65 keypair generated — pk={} bytes, seed=32 bytes",
            public.len(),
        );

        Self { seed, public }
    }

    /// Reconstruct the expanded signing key from the stored seed.
    fn signing_key(&self) -> ml_dsa::SigningKey<MlDsa65> {
        let seed_array = ml_dsa::B32::from(self.seed);
        MlDsa65::from_seed(&seed_array)
    }

    /// Serialize the public key in SSH wire format.
    ///
    /// ```text
    /// string    "ml-dsa-65"
    /// string    public_key (1952 bytes)
    /// ```
    pub fn public_key_blob(&self) -> Vec<u8> {
        let mut w = SshWriter::new();
        w.write_string_utf8("ml-dsa-65");
        w.write_string(&self.public);
        w.into_bytes()
    }

    /// Sign data and return the signature in SSH wire format.
    pub fn sign(&self, data: &[u8]) -> Vec<u8> {
        log::debug!("hostkey: signing {} bytes with ML-DSA-65", data.len());

        // Real ML-DSA-65 signing via ml-dsa crate (deterministic mode)
        let sk = self.signing_key();
        let sig: ml_dsa::Signature<MlDsa65> = sk.sign(data);
        let sig_bytes = sig.encode();

        let mut w = SshWriter::new();
        w.write_string_utf8("ml-dsa-65");
        w.write_string(sig_bytes.as_slice());
        w.into_bytes()
    }

    /// Verify an ML-DSA-65 signature.
    pub fn verify(public_key: &[u8], data: &[u8], signature: &[u8]) -> bool {
        log::debug!(
            "hostkey: verifying ML-DSA-65 signature — pk={} bytes, data={} bytes, sig={} bytes",
            public_key.len(),
            data.len(),
            signature.len(),
        );

        // Decode the verifying key
        if public_key.len() != MLDSA65_PK_SIZE {
            log::error!("hostkey: ML-DSA-65 public key wrong size: {} (expected {})", public_key.len(), MLDSA65_PK_SIZE);
            return false;
        }
        let vk_enc = match MlDsaEncodedVerifyingKey::<MlDsa65>::try_from(public_key) {
            Ok(enc) => enc,
            Err(_) => {
                log::error!("hostkey: ML-DSA-65 public key encoding error");
                return false;
            }
        };
        let vk = MlDsaVerifyingKey::<MlDsa65>::decode(&vk_enc);

        // Decode the signature
        if signature.len() != MLDSA65_SIG_SIZE {
            log::error!("hostkey: ML-DSA-65 signature wrong size: {} (expected {})", signature.len(), MLDSA65_SIG_SIZE);
            return false;
        }
        let sig_enc = match MlDsaEncodedSignature::<MlDsa65>::try_from(signature) {
            Ok(enc) => enc,
            Err(_) => {
                log::error!("hostkey: ML-DSA-65 signature encoding error");
                return false;
            }
        };
        let sig = match ml_dsa::Signature::<MlDsa65>::decode(&sig_enc) {
            Some(s) => s,
            None => {
                log::error!("hostkey: ML-DSA-65 signature decode failed");
                return false;
            }
        };

        // Verify using the real ml-dsa verifier
        match vk.verify(data, &sig) {
            Ok(()) => {
                log::debug!("hostkey: ML-DSA-65 signature verified successfully");
                true
            }
            Err(e) => {
                log::debug!("hostkey: ML-DSA-65 signature verification failed: {}", e);
                false
            }
        }
    }

    /// Get the raw public key bytes.
    pub fn public_key_bytes(&self) -> &[u8] {
        &self.public
    }

    /// Serialize for persistence (seed + public key).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut w = SshWriter::new();
        w.write_string(&self.seed);
        w.write_string(&self.public);
        w.into_bytes()
    }

    /// Deserialize from persistence.
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        let mut r = crate::wire::SshReader::new(data);
        let seed_bytes = r.read_string_raw().ok()?;
        let public = r.read_string_raw().ok()?.to_vec();

        if seed_bytes.len() != 32 {
            log::error!(
                "hostkey: ML-DSA-65 seed wrong size — got {}, expected 32",
                seed_bytes.len(),
            );
            return None;
        }
        if public.len() != MLDSA65_PK_SIZE {
            log::error!(
                "hostkey: ML-DSA-65 public key wrong size — got {}, expected {}",
                public.len(),
                MLDSA65_PK_SIZE,
            );
            return None;
        }

        let mut seed = [0u8; 32];
        seed.copy_from_slice(seed_bytes);

        log::debug!("hostkey: ML-DSA-65 keypair loaded from persistence");
        Some(Self { seed, public })
    }
}

// ---------------------------------------------------------------------------
// Hybrid host key: ML-DSA-65 + Ed25519
// ---------------------------------------------------------------------------

/// A hybrid host key combining ML-DSA-65 and Ed25519.
/// Both keys are presented together; both signatures are produced during KEX.
pub struct HybridHostKey {
    /// The Ed25519 component.
    pub ed25519: Ed25519HostKey,
    /// The ML-DSA-65 component.
    pub ml_dsa: MlDsa65HostKey,
}

impl HybridHostKey {
    /// Generate a new hybrid host keypair.
    pub fn generate(rng: &mut dyn FnMut(&mut [u8])) -> Self {
        log::info!("hostkey: generating hybrid ML-DSA-65 + Ed25519 host keypair");
        Self {
            ed25519: Ed25519HostKey::generate(rng),
            ml_dsa: MlDsa65HostKey::generate(rng),
        }
    }

    /// Serialize the hybrid public key in SSH wire format.
    ///
    /// ```text
    /// string    "mlkem768-ed25519@openssh.com"
    /// string    ed25519_public_key (32 bytes)
    /// string    ml_dsa_65_public_key (1952 bytes)
    /// ```
    pub fn public_key_blob(&self) -> Vec<u8> {
        let mut w = SshWriter::new();
        w.write_string_utf8(SSH_MLDSA65_ED25519);
        w.write_string(self.ed25519.public_key_bytes());
        w.write_string(self.ml_dsa.public_key_bytes());
        w.into_bytes()
    }

    /// Dual-sign: produce both Ed25519 and ML-DSA-65 signatures over data.
    ///
    /// ```text
    /// string    "mlkem768-ed25519@openssh.com"
    /// string    ed25519_signature (64 bytes)
    /// string    ml_dsa_65_signature (3309 bytes)
    /// ```
    pub fn sign(&self, data: &[u8]) -> Vec<u8> {
        log::info!("hostkey: dual-signing {} bytes (Ed25519 + ML-DSA-65)", data.len());

        // Get raw signatures (without their own type prefixes)
        // For the hybrid format, we embed both raw signatures.
        let ed25519_sig = self.ed25519.sign(data);
        let ml_dsa_sig = self.ml_dsa.sign(data);

        let mut w = SshWriter::new();
        w.write_string_utf8(SSH_MLDSA65_ED25519);
        w.write_string(&ed25519_sig);
        w.write_string(&ml_dsa_sig);
        w.into_bytes()
    }

    /// Serialize for persistence.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut w = SshWriter::new();
        let ed_bytes = self.ed25519.to_bytes();
        let ml_bytes = self.ml_dsa.to_bytes();
        w.write_string(&ed_bytes);
        w.write_string(&ml_bytes);
        w.into_bytes()
    }

    /// Deserialize from persistence.
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        let mut r = crate::wire::SshReader::new(data);
        let ed_bytes = r.read_string_raw().ok()?;
        let ml_bytes = r.read_string_raw().ok()?;
        let ed25519 = Ed25519HostKey::from_bytes(ed_bytes)?;
        let ml_dsa = MlDsa65HostKey::from_bytes(ml_bytes)?;
        log::debug!("hostkey: hybrid keypair loaded from persistence");
        Some(Self { ed25519, ml_dsa })
    }
}
