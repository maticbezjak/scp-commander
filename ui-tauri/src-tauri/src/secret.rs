// Secure password storage via the OS keychain (Keychain / Credential Manager /
// kernel keyutils), keyed by site name. Passwords never touch the sites JSON.

use keyring::{Entry, Error};

const SERVICE: &str = "net.manto.scpcommander";

#[tauri::command]
pub fn secret_set(account: String, password: String) -> Result<(), String> {
    Entry::new(SERVICE, &account)
        .and_then(|e| e.set_password(&password))
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn secret_get(account: String) -> Option<String> {
    Entry::new(SERVICE, &account).ok()?.get_password().ok()
}

#[tauri::command]
pub fn secret_delete(account: String) -> Result<(), String> {
    let entry = Entry::new(SERVICE, &account).map_err(|e| e.to_string())?;
    match entry.delete_credential() {
        Ok(()) | Err(Error::NoEntry) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}
