// Copyright 2019-2023 Tauri Programme within The Commons Conservancy
// Copyright 2023-2023 CrabNebula Ltd.
// SPDX-License-Identifier: Apache-2.0
// SPDX-License-Identifier: MIT

//! File singing and signing keys creation and decoding.

use std::{
    fmt::Debug,
    fs::{self, OpenOptions},
    io::{BufReader, Write},
    path::{Path, PathBuf},
    str,
    time::{SystemTime, UNIX_EPOCH},
};

use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};

use crate::{
    util::{self, PathExt},
    Error,
};

/// A public and secret key pair.
#[derive(Clone, Debug)]
pub struct KeyPair {
    /// Publick key
    pub pk: String,
    /// Secret key
    pub sk: String,
}

/// Generates a new signing key. If `password` is `None`, it will prompt
/// the user for a password, so if you want to skip the prompt, specify and
/// empty string as the password.
#[tracing::instrument(level = "trace")]
pub fn generate_key(password: Option<String>) -> crate::Result<KeyPair> {
    let minisign::KeyPair { pk, sk } = minisign::KeyPair::generate_encrypted_keypair(password)?;

    let pk_box_str = pk.to_box()?.to_string();
    let sk_box_str = sk.to_box(None)?.to_string();

    let encoded_pk = base64::engine::general_purpose::STANDARD.encode(pk_box_str);
    let encoded_sk = base64::engine::general_purpose::STANDARD.encode(sk_box_str);

    Ok(KeyPair {
        pk: encoded_pk,
        sk: encoded_sk,
    })
}

fn decode_base64(base64_key: &str) -> crate::Result<String> {
    let decoded_str = &base64::engine::general_purpose::STANDARD.decode(base64_key)?[..];
    Ok(String::from(str::from_utf8(decoded_str)?))
}

/// Decodes a private key using the specified password.
#[tracing::instrument(level = "trace")]
pub fn decode_private_key(
    private_key: &str,
    password: Option<&str>,
) -> crate::Result<minisign::SecretKey> {
    let decoded_secret = decode_base64(private_key)?;
    let sk_box = minisign::SecretKeyBox::from_string(&decoded_secret)?;
    let sk = sk_box.into_secret_key(password.map(Into::into))?;
    Ok(sk)
}

/// Saves a [`KeyPair`] to disk.
#[tracing::instrument(level = "trace")]
pub fn save_keypair<P: AsRef<Path> + Debug>(
    keypair: &KeyPair,
    path: P,
    force: bool,
) -> crate::Result<(PathBuf, PathBuf)> {
    let path = path.as_ref();

    let pubkey_path = format!("{}.pub", path.display());
    let pk_path = Path::new(&pubkey_path);

    if path.exists() {
        if !force {
            return Err(Error::SigningKeyExists(path.to_path_buf()));
        } else {
            fs::remove_file(path).map_err(|e| Error::IoWithPath(path.to_path_buf(), e))?;
        }
    }

    if pk_path.exists() {
        fs::remove_file(pk_path).map_err(|e| Error::IoWithPath(pk_path.to_path_buf(), e))?;
    }

    let mut sk_writer = util::create_file(path)?;
    write!(sk_writer, "{}", keypair.sk)?;
    sk_writer.flush()?;

    let mut pk_writer = util::create_file(pk_path)?;
    write!(pk_writer, "{}", keypair.pk)?;
    pk_writer.flush()?;

    Ok((
        dunce::canonicalize(path).map_err(|e| Error::IoWithPath(path.to_path_buf(), e))?,
        dunce::canonicalize(pk_path).map_err(|e| Error::IoWithPath(pk_path.to_path_buf(), e))?,
    ))
}

/// Signing configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[non_exhaustive]
pub struct SigningConfig {
    /// The private key to use for signing.
    pub private_key: String,
    /// The private key password.
    ///
    /// If `None`, user will be prompted to write a password.
    /// You can skip the prompt by specifying an empty string.
    pub password: Option<String>,
}

impl SigningConfig {
    /// Creates a new [`SigningConfig`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the private key to use for signing.
    pub fn private_key<S: Into<String>>(mut self, private_key: S) -> Self {
        self.private_key = private_key.into();
        self
    }

    /// Set the private key password.
    pub fn password<S: Into<String>>(mut self, password: S) -> Self {
        self.password.replace(password.into());

        self
    }
}

/// Signs a specified file using the specified signing configuration.
#[tracing::instrument(level = "trace")]
pub fn sign_file<P: AsRef<Path> + Debug>(
    config: &SigningConfig,
    path: P,
) -> crate::Result<(PathBuf, String)> {
    let secret_key = decode_private_key(&config.private_key, config.password.as_deref())?;
    sign_file_with_secret_key(&secret_key, path)
}

/// Signs a specified file using an already decoded secret key.
#[tracing::instrument(level = "trace")]
pub fn sign_file_with_secret_key<P: AsRef<Path> + Debug>(
    secret_key: &minisign::SecretKey,
    path: P,
) -> crate::Result<(PathBuf, String)> {
    let path = path.as_ref();
    let signature_path = path.with_additional_extension("sig");
    let signature_path = dunce::simplified(&signature_path);

    let mut signature_box_writer = util::create_file(signature_path)?;
    let start = SystemTime::now();
    let since_epoch = start.duration_since(UNIX_EPOCH)?.as_secs();
    let trusted_comment = format!(
        "timestamp:{}\tfile:{}",
        since_epoch,
        path.file_name()
            .ok_or_else(|| crate::Error::FailedToExtractFilename(path.to_path_buf()))?
            .to_string_lossy()
    );

    let file = OpenOptions::new()
        .read(true)
        .open(path)
        .map_err(|e| Error::IoWithPath(path.to_path_buf(), e))?;
    let file_reader = BufReader::new(file);

    let signature_box = minisign::sign(
        None,
        secret_key,
        file_reader,
        Some(trusted_comment.as_str()),
        Some("signature from cargo-packager secret key"),
    )?;

    let encoded_signature = STANDARD.encode(signature_box.to_string());
    signature_box_writer.write_all(encoded_signature.as_bytes())?;
    signature_box_writer.flush()?;

    Ok((
        dunce::canonicalize(signature_path)
            .map_err(|e| crate::Error::IoWithPath(path.to_path_buf(), e))?,
        encoded_signature,
    ))
}
