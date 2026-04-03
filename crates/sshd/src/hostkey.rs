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

use ml_dsa::{MlDsa65, ExpandedSigningKey as MlDsaExpandedSigningKey, VerifyingKey as MlDsaVerifyingKey};
use ml_dsa::{EncodedSignature as MlDsaEncodedSignature, EncodedVerifyingKey as MlDsaEncodedVerifyingKey};
use ml_dsa::signature::Signer as MlDsaSigner;
use ml_dsa::signature::Verifier as MlDsaVerifier;
use ml_dsa::KeyGen;

use crate::kex::FnRng;
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
    /// Ed25519 secret key (32 bytes seed, or 64 bytes expanded).
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

        // TODO: Wire up ed25519_dalek:
        //   use ed25519_dalek::SigningKey;
        //   let signing_key = SigningKey::from_bytes(&secret);
        //   let public = signing_key.verifying_key().to_bytes();
        //
        // Placeholder: derive public key via hash (NOT cryptographically correct,
        // just to validate the protocol plumbing).
        let mut hasher = Sha256::new();
        hasher.update(&secret);
        let h = hasher.finalize();
        let mut public = [0u8; 32];
        public.copy_from_slice(&h);

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

        // TODO: Wire up ed25519_dalek:
        //   use ed25519_dalek::SigningKey;
        //   let signing_key = SigningKey::from_bytes(&self.secret);
        //   let sig = signing_key.sign(data);
        //   let sig_bytes = sig.to_bytes();

        // Placeholder signature (HMAC-SHA256 of data with secret as key)
        let mut hasher = Sha256::new();
        hasher.update(&self.secret);
        hasher.update(data);
        let h1 = hasher.finalize();

        let mut hasher2 = Sha256::new();
        hasher2.update(h1);
        hasher2.update(data);
        let h2 = hasher2.finalize();

        let mut sig_bytes = [0u8; 64];
        sig_bytes[..32].copy_from_slice(&h1);
        sig_bytes[32..].copy_from_slice(&h2);

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

        // TODO: Wire up ed25519_dalek:
        //   use ed25519_dalek::VerifyingKey;
        //   let vk = VerifyingKey::from_bytes(public_key)?;
        //   let sig = ed25519_dalek::Signature::from_bytes(signature)?;
        //   vk.verify(data, &sig).is_ok()
        let _ = (public_key, data, signature);
        log::warn!("hostkey: Ed25519 verify is a placeholder — always returns false");
        false
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
        log::debug!("hostkey: Ed25519 keypair loaded from persistence");
        Some(Self { secret, public })
    }
}

// ---------------------------------------------------------------------------
// ML-DSA-65 host key (post-quantum)
// ---------------------------------------------------------------------------

/// An ML-DSA-65 host keypair (FIPS 204 / CRYSTALS-Dilithium).
pub struct MlDsa65HostKey {
    /// ML-DSA-65 secret/signing key.
    secret: Vec<u8>,
    /// ML-DSA-65 public/verifying key.
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

        // TODO: Wire up ml_dsa crate:
        //   use ml_dsa::MlDsa65;
        //   let (sk, pk) = MlDsa65::key_gen(&mut rng);

        // Placeholder keys
        let mut secret = alloc::vec![0u8; MLDSA65_SK_SIZE];
        rng(&mut secret);
        let mut public = alloc::vec![0u8; MLDSA65_PK_SIZE];
        rng(&mut public);

        log::debug!(
            "hostkey: ML-DSA-65 keypair generated — pk={} bytes, sk={} bytes",
            public.len(),
            secret.len(),
        );

        Self { secret, public }
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

        // TODO: Wire up ml_dsa crate:
        //   use ml_dsa::MlDsa65;
        //   let sig = MlDsa65::sign(&self.secret, data);

        // Placeholder: hash-based "signature"
        let mut sig = alloc::vec![0u8; MLDSA65_SIG_SIZE];
        let mut hasher = Sha256::new();
        hasher.update(&self.secret[..32]);
        hasher.update(data);
        let h = hasher.finalize();
        sig[..32].copy_from_slice(&h);

        let mut w = SshWriter::new();
        w.write_string_utf8("ml-dsa-65");
        w.write_string(&sig);
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

        // TODO: Wire up ml_dsa crate
        let _ = (public_key, data, signature);
        log::warn!("hostkey: ML-DSA-65 verify is a placeholder — always returns false");
        false
    }

    /// Get the raw public key bytes.
    pub fn public_key_bytes(&self) -> &[u8] {
        &self.public
    }

    /// Serialize for persistence.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut w = SshWriter::new();
        w.write_string(&self.secret);
        w.write_string(&self.public);
        w.into_bytes()
    }

    /// Deserialize from persistence.
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        let mut r = crate::wire::SshReader::new(data);
        let secret = r.read_string_raw().ok()?.to_vec();
        let public = r.read_string_raw().ok()?.to_vec();
        if secret.len() != MLDSA65_SK_SIZE || public.len() != MLDSA65_PK_SIZE {
            log::error!(
                "hostkey: ML-DSA-65 key sizes wrong — sk={}, pk={}",
                secret.len(),
                public.len(),
            );
            return None;
        }
        log::debug!("hostkey: ML-DSA-65 keypair loaded from persistence");
        Some(Self { secret, public })
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
