//! Signing for the authority. A [`Signer`] signs and shows its public key. It never gives up a
//! private key, because for the real one there is none to give: it is in a KMS.

use std::path::PathBuf;
use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use rcgen::{KeyPair, RemoteKeyPair, SignatureAlgorithm};
use ring::rand::SystemRandom;
use ring::signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair, KeyPair as _};

use crate::error::PkiError;
use crate::runner::Runner;

/// Signs with ECDSA P-256 and SHA-256, and says what its public key is.
pub trait Signer: Send + Sync {
    /// The public key as an uncompressed point: 65 bytes, starting with 0x04.
    fn public_key(&self) -> &[u8];
    /// A DER-encoded ECDSA signature over `message`.
    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, PkiError>;
}

/// A key held in this process's memory. For tests and rehearsals only: the real authority is
/// [`KmsSigner`], whose key nobody can export.
pub struct MemorySigner {
    key: EcdsaKeyPair,
    rng: SystemRandom,
    public: Vec<u8>,
}

impl MemorySigner {
    pub fn new() -> Result<Self, PkiError> {
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng)
            .map_err(|_| PkiError::Signer("a key could not be made"))?;
        let key = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), &rng)
            .map_err(|_| PkiError::Signer("a key could not be read"))?;
        let public = key.public_key().as_ref().to_vec();
        Ok(MemorySigner { key, rng, public })
    }
}

impl Signer for MemorySigner {
    fn public_key(&self) -> &[u8] {
        &self.public
    }
    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, PkiError> {
        self.key
            .sign(&self.rng, message)
            .map(|s| s.as_ref().to_vec())
            .map_err(|_| PkiError::Signer("signing failed"))
    }
}

/// The fixed start of a P-256 `SubjectPublicKeyInfo`, up to the point.
const P256_SPKI_PREFIX: [u8; 26] = [
    0x30, 0x59, 0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01, 0x06, 0x08, 0x2a,
    0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07, 0x03, 0x42, 0x00,
];

/// The point inside a P-256 `SubjectPublicKeyInfo`, as KMS gives it.
pub fn point_from_spki(spki: &[u8]) -> Result<Vec<u8>, PkiError> {
    if spki.len() == 91 && spki[..26] == P256_SPKI_PREFIX && spki[26] == 0x04 {
        Ok(spki[26..].to_vec())
    } else {
        Err(PkiError::Signer("the key is not an ECC NIST P-256 key"))
    }
}

/// Signs through AWS KMS, with the `aws` command line tool. The key is an ECC NIST P-256 key used
/// for signing and verifying. Nothing leaves KMS but signatures and the public key.
pub struct KmsSigner<R: Runner> {
    runner: R,
    key_id: String,
    work: PathBuf,
    public: Vec<u8>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "PascalCase")]
struct PublicKeyAnswer {
    public_key: String,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "PascalCase")]
struct SignAnswer {
    signature: String,
}

impl<R: Runner> KmsSigner<R> {
    /// `work` is a private directory for the one small file KMS is given to sign each time.
    pub fn new(runner: R, key_id: &str, work: PathBuf) -> Result<Self, PkiError> {
        let out = runner.run(
            "aws",
            &[
                "kms".into(),
                "get-public-key".into(),
                "--key-id".into(),
                key_id.into(),
                "--output".into(),
                "json".into(),
            ],
            "reading the authority's public key",
        )?;
        let answer: PublicKeyAnswer = serde_json::from_slice(&out)
            .map_err(|_| PkiError::Unreadable("reading the authority's public key"))?;
        let spki = STANDARD
            .decode(answer.public_key)
            .map_err(|_| PkiError::Unreadable("reading the authority's public key"))?;
        Ok(KmsSigner {
            runner,
            key_id: key_id.to_owned(),
            work,
            public: point_from_spki(&spki)?,
        })
    }
}

impl<R: Runner> Signer for KmsSigner<R> {
    fn public_key(&self) -> &[u8] {
        &self.public
    }

    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, PkiError> {
        std::fs::create_dir_all(&self.work)
            .map_err(|_| PkiError::Signer("no place to put the message"))?;
        let file = self.work.join("to-sign");
        std::fs::write(&file, message)
            .map_err(|_| PkiError::Signer("no place to put the message"))?;
        let out = self.runner.run(
            "aws",
            &[
                "kms".into(),
                "sign".into(),
                "--key-id".into(),
                self.key_id.clone(),
                "--message".into(),
                format!("fileb://{}", file.display()),
                "--message-type".into(),
                "RAW".into(),
                "--signing-algorithm".into(),
                "ECDSA_SHA_256".into(),
                "--output".into(),
                "json".into(),
            ],
            "signing a certificate",
        );
        let _ = std::fs::remove_file(&file);
        let answer: SignAnswer = serde_json::from_slice(&out?)
            .map_err(|_| PkiError::Unreadable("signing a certificate"))?;
        STANDARD
            .decode(answer.signature)
            .map_err(|_| PkiError::Unreadable("signing a certificate"))
    }
}

/// Lets rcgen sign through a [`Signer`].
struct Remote(Arc<dyn Signer>);

impl RemoteKeyPair for Remote {
    fn public_key(&self) -> &[u8] {
        self.0.public_key()
    }
    fn sign(&self, msg: &[u8]) -> Result<Vec<u8>, rcgen::Error> {
        self.0.sign(msg).map_err(|_| rcgen::Error::RemoteKeyError)
    }
    fn algorithm(&self) -> &'static SignatureAlgorithm {
        &rcgen::PKCS_ECDSA_P256_SHA256
    }
}

/// An rcgen key that signs through the signer.
pub fn key_pair(signer: Arc<dyn Signer>) -> Result<KeyPair, PkiError> {
    KeyPair::from_remote(Box::new(Remote(signer)))
        .map_err(|_| PkiError::Signer("the key was not accepted"))
}
