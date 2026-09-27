//! Splitter Pro licences, checked offline.
//!
//! mayorana.ch signs each licence when it is bought (api0's store, `payment::license`)
//! with a private Ed25519 key; the app holds the public half and checks the signature, so
//! no account, network or activation is involved. The key a buyer pastes is
//!
//! ```text
//! <base64url(payload JSON)>.<base64url(signature of those bytes)>
//! ```
//!
//! with the payload `{"v":1,"id","product","edition","email","issued","updates_until"}`.
//! A licence unlocks every release dated up to `updates_until`, and keeps unlocking
//! those releases after that day: only newer releases ask for a renewal.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ed25519_dalek::{Signature, VerifyingKey};
use serde::Deserialize;

/// The product name in every Splitter licence.
pub const PRODUCT: &str = "splitter";

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct License {
    pub id: String,
    pub product: String,
    pub edition: String,
    pub email: String,
    /// `YYYY-MM-DD`.
    pub issued: String,
    /// `YYYY-MM-DD`: the last release date this licence unlocks.
    pub updates_until: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LicenseError {
    /// Not something a licence key looks like (truncated, or not a key at all).
    Malformed,
    /// Well-formed, but not signed by mayorana.ch: edited, or made up.
    BadSignature,
    /// A genuine licence, for another app.
    OtherProduct(String),
}

impl std::fmt::Display for LicenseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed => write!(f, "This isn't a complete licence key. Copy the whole key from the email."),
            Self::BadSignature => write!(f, "This licence key isn't valid."),
            Self::OtherProduct(p) => write!(f, "This is a licence for {p}, not Splitter."),
        }
    }
}

/// The licence in `key`, if `public_key` signed it and it is for Splitter. Whitespace is
/// ignored, since keys pasted from an email are often wrapped.
pub fn verify(key: &str, public_key: &[u8; 32]) -> Result<License, LicenseError> {
    let key: String = key.chars().filter(|c| !c.is_whitespace()).collect();
    let (body, signature) = key.split_once('.').ok_or(LicenseError::Malformed)?;
    let payload = URL_SAFE_NO_PAD.decode(body).map_err(|_| LicenseError::Malformed)?;
    let signature = URL_SAFE_NO_PAD.decode(signature).map_err(|_| LicenseError::Malformed)?;
    let signature = Signature::from_slice(&signature).map_err(|_| LicenseError::Malformed)?;
    let verifying = VerifyingKey::from_bytes(public_key).map_err(|_| LicenseError::BadSignature)?;
    verifying.verify_strict(&payload, &signature).map_err(|_| LicenseError::BadSignature)?;
    let license: License = serde_json::from_slice(&payload).map_err(|_| LicenseError::Malformed)?;
    if license.product != PRODUCT {
        return Err(LicenseError::OtherProduct(license.product));
    }
    Ok(license)
}

impl License {
    /// Whether this licence unlocks a release made on `release_date` (`YYYY-MM-DD`).
    /// ISO dates compare correctly as strings.
    pub fn covers(&self, release_date: &str) -> bool {
        release_date <= self.updates_until.as_str()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn sign(json: &str, key: &SigningKey) -> String {
        format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(json),
            URL_SAFE_NO_PAD.encode(key.sign(json.as_bytes()).to_bytes())
        )
    }

    const PAYLOAD: &str = r#"{"v":1,"id":"lic_1","product":"splitter","edition":"pro","email":"anna@band.ch","issued":"2026-09-27","updates_until":"2027-09-27"}"#;

    fn keys() -> (SigningKey, [u8; 32]) {
        let signer = SigningKey::from_bytes(&[7u8; 32]);
        let public = signer.verifying_key().to_bytes();
        (signer, public)
    }

    #[test]
    fn a_signed_key_is_accepted_even_when_wrapped_by_an_email() {
        let (signer, public) = keys();
        let key = sign(PAYLOAD, &signer);
        let (a, b) = key.split_at(40);
        let license = verify(&format!("  {a}\n{b}\n"), &public).unwrap();
        assert_eq!(license.email, "anna@band.ch");
        assert_eq!(license.edition, "pro");
    }

    #[test]
    fn a_key_signed_by_anyone_else_or_edited_is_refused() {
        let (_, public) = keys();
        let forger = SigningKey::from_bytes(&[8u8; 32]);
        assert_eq!(verify(&sign(PAYLOAD, &forger), &public), Err(LicenseError::BadSignature));

        let (signer, public) = keys();
        let key = sign(PAYLOAD, &signer);
        let (_, signature) = key.split_once('.').unwrap();
        let edited = URL_SAFE_NO_PAD.encode(PAYLOAD.replace("2027-09-27", "2099-12-31"));
        assert_eq!(verify(&format!("{edited}.{signature}"), &public), Err(LicenseError::BadSignature));
    }

    #[test]
    fn junk_and_truncated_keys_are_malformed() {
        let (signer, public) = keys();
        let key = sign(PAYLOAD, &signer);
        assert_eq!(verify("", &public), Err(LicenseError::Malformed));
        assert_eq!(verify("hello", &public), Err(LicenseError::Malformed));
        assert_eq!(verify(&key[..key.len() - 10], &public), Err(LicenseError::Malformed));
    }

    #[test]
    fn a_licence_for_another_app_is_not_a_splitter_licence() {
        let (signer, public) = keys();
        let key = sign(&PAYLOAD.replace("splitter", "other"), &signer);
        assert_eq!(verify(&key, &public), Err(LicenseError::OtherProduct("other".into())));
    }

    /// Issued by api0's store with the test signing key (seed [7; 32]); its own test
    /// pins the same string, so a format change on either side fails one of them.
    const SERVER_ISSUED: &str = "eyJ2IjoxLCJpZCI6ImxpY19maXh0dXJlIiwicHJvZHVjdCI6InNwbGl0dGVyIiwiZWRpdGlvbiI6InBybyIsImVtYWlsIjoiYW5uYUBiYW5kLmNoIiwiaXNzdWVkIjoiMjAyNi0wOS0yNyIsInVwZGF0ZXNfdW50aWwiOiIyMDI3LTA5LTI3In0.pgBj4R164hG5dGk-jfLUKINF7zOBSPO8to5PImF8QoDp4dRHGgX67ORXy_eC9Pyd9CqUxGj_r6_ETd1oHWI3BQ";

    #[test]
    fn a_key_issued_by_the_server_verifies() {
        let (_, public) = keys();
        let license = verify(SERVER_ISSUED, &public).unwrap();
        assert_eq!(license.id, "lic_fixture");
        assert_eq!(license.updates_until, "2027-09-27");
    }

    #[test]
    fn covers_releases_up_to_the_last_update_day() {
        let (signer, public) = keys();
        let license = verify(&sign(PAYLOAD, &signer), &public).unwrap();
        assert!(license.covers("2026-10-01"));
        assert!(license.covers("2027-09-27"));
        assert!(!license.covers("2027-09-28"));
    }
}
