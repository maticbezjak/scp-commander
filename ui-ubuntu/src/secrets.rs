//! Password storage via the freedesktop Secret Service (GNOME Keyring /
//! KWallet). Uses the blocking API — calls are quick D-Bus roundtrips and only
//! happen on explicit user actions (save/load/delete a site).
//!
//! On systems without a Secret Service daemon these calls just return errors,
//! which the UI reports as status text — sites still work, minus passwords.

use std::collections::HashMap;

use secret_service::blocking::SecretService;
use secret_service::EncryptionType;

const ATTR: &str = "scp-commander-site";

/// Account key: proto://user@host:port (matches the macOS Keychain scheme).
pub fn account(proto: &str, user: &str, host: &str, port: &str) -> String {
    format!("{}://{}@{}:{}", proto.to_lowercase(), user, host, port)
}

pub fn save(account: &str, password: &str) -> Result<(), String> {
    let ss = SecretService::connect(EncryptionType::Dh).map_err(|e| e.to_string())?;
    let collection = ss.get_default_collection().map_err(|e| e.to_string())?;
    collection
        .create_item(
            &format!("SCP Commander: {account}"),
            HashMap::from([(ATTR, account)]),
            password.as_bytes(),
            true, // replace
            "text/plain",
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn load(account: &str) -> Option<String> {
    let ss = SecretService::connect(EncryptionType::Dh).ok()?;
    let search = ss
        .search_items(HashMap::from([(ATTR, account)]))
        .ok()?;
    let item = search.unlocked.first()?;
    let secret = item.get_secret().ok()?;
    String::from_utf8(secret).ok()
}

pub fn delete(account: &str) {
    let Ok(ss) = SecretService::connect(EncryptionType::Dh) else { return };
    let Ok(search) = ss.search_items(HashMap::from([(ATTR, account)])) else { return };
    for item in search.unlocked {
        let _ = item.delete();
    }
}
