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

pub fn store_token(token: &str) -> Result<()> {
    entry()?
        .set_password(token)
        .map_err(|e| AppError::Keychain(format!("Could not save the token to the keychain: {e}")))
}

/// `Ok(None)` means no token is stored, which is an ordinary first-run state
/// rather than a failure.
pub fn load_token() -> Result<Option<String>> {
    match entry()?.get_password() {
        Ok(token) => Ok(Some(token)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(AppError::Keychain(format!(
            "Could not read the token from the keychain: {e}"
        ))),
    }
}

pub fn clear_token() -> Result<()> {
    match entry()?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(AppError::Keychain(format!(
            "Could not remove the token from the keychain: {e}"
        ))),
    }
}
