//! Subresource-integrity strings as npm writes them in `package-lock.json`
//! (`sha512-<base64>`, optionally several space-separated hashes).

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha384, Sha512};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Algorithm {
    Sha1,
    Sha256,
    Sha384,
    Sha512,
}

impl Algorithm {
    fn parse(name: &str) -> Option<Self> {
        match name {
            "sha1" => Some(Self::Sha1),
            "sha256" => Some(Self::Sha256),
            "sha384" => Some(Self::Sha384),
            "sha512" => Some(Self::Sha512),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Sha1 => "sha1",
            Self::Sha256 => "sha256",
            Self::Sha384 => "sha384",
            Self::Sha512 => "sha512",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Integrity {
    pub algorithm: Algorithm,
    pub digest: Vec<u8>,
}

impl Integrity {
    /// Parses an SRI string and keeps the strongest supported hash, as ssri
    /// does when it verifies content.
    pub fn parse(sri: &str) -> Result<Self, String> {
        let mut best: Option<Integrity> = None;
        for token in sri.split_whitespace() {
            let Some((name, rest)) = token.split_once('-') else {
                continue;
            };
            let Some(algorithm) = Algorithm::parse(name) else {
                continue;
            };
            // Options after `?` are allowed by the SRI grammar and ignored.
            let b64 = rest.split('?').next().unwrap_or_default();
            let digest = STANDARD
                .decode(b64)
                .map_err(|_| format!("invalid base64 in integrity {sri:?}"))?;
            if best.as_ref().is_none_or(|b| algorithm > b.algorithm) {
                best = Some(Integrity { algorithm, digest });
            }
        }
        best.ok_or_else(|| format!("no supported hash in integrity {sri:?}"))
    }

    pub fn hasher(&self) -> Hasher {
        match self.algorithm {
            Algorithm::Sha1 => Hasher::Sha1(Sha1::new()),
            Algorithm::Sha256 => Hasher::Sha256(Sha256::new()),
            Algorithm::Sha384 => Hasher::Sha384(Sha384::new()),
            Algorithm::Sha512 => Hasher::Sha512(Sha512::new()),
        }
    }

    pub fn matches(&self, digest: &[u8]) -> bool {
        // Not secret material, so a plain comparison is fine.
        self.digest == digest
    }

    /// A filesystem-safe key for the content-addressable store.
    pub fn store_key(&self) -> String {
        let mut key = String::with_capacity(8 + self.digest.len() * 2);
        key.push_str(self.algorithm.name());
        key.push('-');
        for byte in &self.digest {
            key.push_str(&format!("{byte:02x}"));
        }
        key
    }

    pub fn to_sri(&self) -> String {
        format!("{}-{}", self.algorithm.name(), STANDARD.encode(&self.digest))
    }
}

pub enum Hasher {
    Sha1(Sha1),
    Sha256(Sha256),
    Sha384(Sha384),
    Sha512(Sha512),
}

impl Hasher {
    pub fn update(&mut self, data: &[u8]) {
        match self {
            Hasher::Sha1(h) => h.update(data),
            Hasher::Sha256(h) => h.update(data),
            Hasher::Sha384(h) => h.update(data),
            Hasher::Sha512(h) => h.update(data),
        }
    }

    pub fn finish(self) -> Vec<u8> {
        match self {
            Hasher::Sha1(h) => h.finalize().to_vec(),
            Hasher::Sha256(h) => h.finalize().to_vec(),
            Hasher::Sha384(h) => h.finalize().to_vec(),
            Hasher::Sha512(h) => h.finalize().to_vec(),
        }
    }
}

pub fn sha512_sri(data: &[u8]) -> String {
    format!("sha512-{}", STANDARD.encode(Sha512::digest(data)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_verifies_sha512() {
        let sri = sha512_sri(b"hello");
        let integrity = Integrity::parse(&sri).unwrap();
        let mut hasher = integrity.hasher();
        hasher.update(b"hello");
        assert!(integrity.matches(&hasher.finish()));
        assert_eq!(integrity.to_sri(), sri);
    }

    #[test]
    fn keeps_strongest_hash() {
        let sri = format!("sha1-AAAA {}", sha512_sri(b"x"));
        assert_eq!(Integrity::parse(&sri).unwrap().algorithm, Algorithm::Sha512);
    }

    #[test]
    fn rejects_unknown() {
        assert!(Integrity::parse("md5-abc").is_err());
    }
}
