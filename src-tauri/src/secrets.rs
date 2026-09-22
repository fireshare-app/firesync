use std::sync::RwLock;

use crate::error::{AppError, Result};

const SERVICE: &str = "app.fireshare.firesync";
const ACCOUNT: &str = "upload-token";

/// The upload token, in the OS credential store.
///
/// It is a bearer credential — anyone holding it can upload as its owner — so it
/// does not go in the config file, where a backup tool, a screen share, or a
/// support request would carry it off. The config file holds the server URL and
/// nothing else that matters.
fn entry() -> Result<keyring::Entry> {
    keyring::Entry::new(SERVICE, ACCOUNT)
        .map_err(|e| AppError::Keychain(format!("Could not open the system keychain: {e}")))
}

fn store(token: &str) -> Result<()> {
    entry()?
        .set_password(token)
        .map_err(|e| AppError::Keychain(format!("Could not save the token to the keychain: {e}")))
}

/// `Ok(None)` means no token is stored, which is an ordinary first-run state
/// rather than a failure.
fn read() -> Result<Option<String>> {
    match entry()?.get_password() {
        Ok(token) => Ok(Some(token)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(AppError::Keychain(format!(
            "Could not read the token from the keychain: {e}"
        ))),
    }
}

fn erase() -> Result<()> {
    match entry()?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(AppError::Keychain(format!(
            "Could not remove the token from the keychain: {e}"
        ))),
    }
}

/// The token, read from the keychain once and held in memory after that.
///
/// The keychain is not a cache. Every read is a real call into the OS
/// credential store, and on macOS an app whose signature the keychain does not
/// recognise is prompted for the user's password on each one — so the upload
/// queue, which checks for a token on every pass of its loop, produced a
/// password prompt every couple of seconds that reappeared no matter how many
/// times it was answered. Even where the OS never prompts, asking a credential
/// store for the same secret thousands of times an hour is not something to do
/// on a timer.
///
/// Writes go through here too, so the copy in memory and the copy in the
/// keychain cannot drift.
pub struct TokenCache {
    cached: RwLock<Option<String>>,
}

impl TokenCache {
    /// Reads the keychain exactly once, at startup. A failure here is not fatal:
    /// the app starts disconnected and the person reconnects, which is a better
    /// outcome than refusing to launch because a keychain was locked.
    pub fn load() -> Self {
        let cached = read().unwrap_or_else(|e| {
            eprintln!("firesync: could not read the stored token ({e}); starting disconnected");
            None
        });
        Self { cached: RwLock::new(cached) }
    }

    pub fn get(&self) -> Option<String> {
        self.cached.read().expect("token cache poisoned").clone()
    }

    pub fn set(&self, token: &str) -> Result<()> {
        store(token)?;
        *self.cached.write().expect("token cache poisoned") = Some(token.to_string());
        Ok(())
    }

    pub fn clear(&self) -> Result<()> {
        erase()?;
        *self.cached.write().expect("token cache poisoned") = None;
        Ok(())
    }
}

impl Default for TokenCache {
    fn default() -> Self {
        Self::load()
    }
}
