//! A client-only rustls [`CryptoProvider`] on graviola, sized for one host.
//!
//! `rustls_graviola::default_provider()` links every key-exchange group and
//! the RSA, Ed25519 and ECDSA signing code (through its key provider), about
//! 1 MB of the library. A client that never presents a certificate needs
//! none of the signing code, and `getsecretary.tech` (Caddy, Let's Encrypt)
//! speaks TLS 1.3 with X25519, an ECDSA P-256 leaf and P-384 intermediates,
//! cross-signed by the RSA ISRG Root X1. So this provider has:
//!
//! - graviola's TLS 1.3 suites (AES-128/256-GCM, ChaCha20-Poly1305) and
//!   X25519, from `rustls_graviola`;
//! - signature verification with ECDSA P-256/P-384 (SHA-256/384) and RSA
//!   PKCS#1/PSS (SHA-256/384/512), on graviola directly (rustls-graviola
//!   keeps its own list private);
//! - no private keys.

use std::sync::Arc;

use graviola::hashing::{Sha256, Sha384};
use graviola::signing::{ecdsa, rsa};
use rustls::crypto::{
    CryptoProvider, GetRandomFailed, KeyProvider, SecureRandom, WebPkiSupportedAlgorithms,
};
use rustls::pki_types::{
    alg_id, AlgorithmIdentifier, InvalidSignature, PrivateKeyDer, SignatureVerificationAlgorithm,
};
use rustls::sign::SigningKey;
use rustls::SignatureScheme;
use rustls_graviola::{kx, suites};

/// The provider for the relay connection.
pub fn provider() -> CryptoProvider {
    CryptoProvider {
        cipher_suites: vec![
            suites::TLS13_AES_128_GCM_SHA256,
            suites::TLS13_CHACHA20_POLY1305_SHA256,
            suites::TLS13_AES_256_GCM_SHA384,
        ],
        kx_groups: vec![&kx::X25519],
        signature_verification_algorithms: ALGORITHMS,
        secure_random: &Rng,
        key_provider: &NoKeys,
    }
}

#[derive(Debug)]
struct Rng;

impl SecureRandom for Rng {
    fn fill(&self, bytes: &mut [u8]) -> Result<(), GetRandomFailed> {
        graviola::random::fill(bytes).map_err(|_| GetRandomFailed)
    }
}

/// The app never presents a client certificate.
#[derive(Debug)]
struct NoKeys;

impl KeyProvider for NoKeys {
    fn load_private_key(
        &self,
        _: PrivateKeyDer<'static>,
    ) -> Result<Arc<dyn SigningKey>, rustls::Error> {
        Err(rustls::Error::General(
            "no private keys in this client".into(),
        ))
    }
}

static ALGORITHMS: WebPkiSupportedAlgorithms = WebPkiSupportedAlgorithms {
    all: &[
        ECDSA_P256_SHA256,
        ECDSA_P256_SHA384,
        ECDSA_P384_SHA384,
        ECDSA_P384_SHA256,
        RSA_PSS_SHA256,
        RSA_PSS_SHA384,
        RSA_PSS_SHA512,
        RSA_PKCS1_SHA256,
        RSA_PKCS1_SHA384,
        RSA_PKCS1_SHA512,
    ],
    mapping: &[
        (
            SignatureScheme::ECDSA_NISTP256_SHA256,
            &[ECDSA_P256_SHA256, ECDSA_P384_SHA256],
        ),
        (
            SignatureScheme::ECDSA_NISTP384_SHA384,
            &[ECDSA_P384_SHA384, ECDSA_P256_SHA384],
        ),
        (SignatureScheme::RSA_PSS_SHA512, &[RSA_PSS_SHA512]),
        (SignatureScheme::RSA_PSS_SHA384, &[RSA_PSS_SHA384]),
        (SignatureScheme::RSA_PSS_SHA256, &[RSA_PSS_SHA256]),
        (SignatureScheme::RSA_PKCS1_SHA512, &[RSA_PKCS1_SHA512]),
        (SignatureScheme::RSA_PKCS1_SHA384, &[RSA_PKCS1_SHA384]),
        (SignatureScheme::RSA_PKCS1_SHA256, &[RSA_PKCS1_SHA256]),
    ],
};

static ECDSA_P256_SHA256: &dyn SignatureVerificationAlgorithm = &EcdsaP256 {
    sig: alg_id::ECDSA_SHA256,
    verify: |k, s, m| k.verify_asn1::<Sha256>(&[m], s),
};
static ECDSA_P256_SHA384: &dyn SignatureVerificationAlgorithm = &EcdsaP256 {
    sig: alg_id::ECDSA_SHA384,
    verify: |k, s, m| k.verify_asn1::<Sha384>(&[m], s),
};
static ECDSA_P384_SHA256: &dyn SignatureVerificationAlgorithm = &EcdsaP384 {
    sig: alg_id::ECDSA_SHA256,
    verify: |k, s, m| k.verify_asn1::<Sha256>(&[m], s),
};
static ECDSA_P384_SHA384: &dyn SignatureVerificationAlgorithm = &EcdsaP384 {
    sig: alg_id::ECDSA_SHA384,
    verify: |k, s, m| k.verify_asn1::<Sha384>(&[m], s),
};

static RSA_PSS_SHA256: &dyn SignatureVerificationAlgorithm = &Rsa {
    sig: alg_id::RSA_PSS_SHA256,
    verify: |k, s, m| k.verify_pss_sha256(s, m),
};
static RSA_PSS_SHA384: &dyn SignatureVerificationAlgorithm = &Rsa {
    sig: alg_id::RSA_PSS_SHA384,
    verify: |k, s, m| k.verify_pss_sha384(s, m),
};
static RSA_PSS_SHA512: &dyn SignatureVerificationAlgorithm = &Rsa {
    sig: alg_id::RSA_PSS_SHA512,
    verify: |k, s, m| k.verify_pss_sha512(s, m),
};
static RSA_PKCS1_SHA256: &dyn SignatureVerificationAlgorithm = &Rsa {
    sig: alg_id::RSA_PKCS1_SHA256,
    verify: |k, s, m| k.verify_pkcs1_sha256(s, m),
};
static RSA_PKCS1_SHA384: &dyn SignatureVerificationAlgorithm = &Rsa {
    sig: alg_id::RSA_PKCS1_SHA384,
    verify: |k, s, m| k.verify_pkcs1_sha384(s, m),
};
static RSA_PKCS1_SHA512: &dyn SignatureVerificationAlgorithm = &Rsa {
    sig: alg_id::RSA_PKCS1_SHA512,
    verify: |k, s, m| k.verify_pkcs1_sha512(s, m),
};

type RsaVerifyFn = fn(&rsa::VerifyingKey, &[u8], &[u8]) -> Result<(), graviola::Error>;

#[derive(Debug)]
struct Rsa {
    sig: AlgorithmIdentifier,
    verify: RsaVerifyFn,
}

impl SignatureVerificationAlgorithm for Rsa {
    fn public_key_alg_id(&self) -> AlgorithmIdentifier {
        alg_id::RSA_ENCRYPTION
    }

    fn signature_alg_id(&self) -> AlgorithmIdentifier {
        self.sig
    }

    fn verify_signature(
        &self,
        public_key: &[u8],
        message: &[u8],
        signature: &[u8],
    ) -> Result<(), InvalidSignature> {
        rsa::VerifyingKey::from_pkcs1_der(public_key)
            .and_then(|k| (self.verify)(&k, signature, message))
            .map_err(|_| InvalidSignature)
    }
}

/// ECDSA verification on one curve (graviola's curve key trait is not
/// exported, so one concrete type per curve).
macro_rules! ecdsa_verify {
    ($name:ident, $curve:ty, $key:expr) => {
        #[derive(Debug)]
        struct $name {
            sig: AlgorithmIdentifier,
            #[allow(clippy::type_complexity)]
            verify: fn(&ecdsa::VerifyingKey<$curve>, &[u8], &[u8]) -> Result<(), graviola::Error>,
        }

        impl SignatureVerificationAlgorithm for $name {
            fn public_key_alg_id(&self) -> AlgorithmIdentifier {
                $key
            }

            fn signature_alg_id(&self) -> AlgorithmIdentifier {
                self.sig
            }

            fn verify_signature(
                &self,
                public_key: &[u8],
                message: &[u8],
                signature: &[u8],
            ) -> Result<(), InvalidSignature> {
                <$curve as ecdsa::Curve>::PublicKey::from_x962_uncompressed(public_key)
                    .and_then(|public_key| {
                        (self.verify)(&ecdsa::VerifyingKey { public_key }, signature, message)
                    })
                    .map_err(|_| InvalidSignature)
            }
        }
    };
}

ecdsa_verify!(EcdsaP256, ecdsa::P256, alg_id::ECDSA_P256);
ecdsa_verify!(EcdsaP384, ecdsa::P384, alg_id::ECDSA_P384);
