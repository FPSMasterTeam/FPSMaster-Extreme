/// Microsoft OAuth → Xbox Live → XSTS → Minecraft authentication, plus on-disk
/// account storage.
///
/// Two ways to obtain a session:
///   * **device-code flow** — the user visits a URL and enters a code while the
///     client polls in the background (no redirect server needed);
///   * **refresh-token flow** — redeem a previously stored MS refresh token
///     directly, skipping the interactive step.
///
/// Microsoft (MSA / consumers) refresh tokens are *rotating*: every redemption
/// returns a NEW refresh token and invalidates the old one, so the freshest
/// token is carried on `Session::refresh_token` and persisted by the caller.
///
/// Client ID `00000000402b5328` is the well-known public "Xbox Live" client id
/// used by open-source launchers (MultiMC / Prism); it targets the `consumers`
/// tenant and needs no client secret for the device-code / refresh grants.
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

/// Default Microsoft Azure application (client) id. NOTE: the legacy 16-hex
/// `00000000402b5328` Minecraft id does NOT work with the v2.0 endpoint used
/// here (it returns AADSTS700016) — it only works on the old `login.live.com`
/// endpoints. Register your own Azure app (personal accounts, public client /
/// device-code enabled, no secret) and set `RECRAFT_MS_CLIENT_ID` to its GUID,
/// or set it to whatever client id issued your refresh token.
pub const DEFAULT_MS_CLIENT_ID: &str = "00000000402b5328";

/// Scopes requested from Microsoft. `offline_access` is what yields a refresh
/// token so the session can be renewed without the interactive flow.
const MS_SCOPES: &str = "XboxLive.signin offline_access";

/// The client id to use, overridable via `RECRAFT_MS_CLIENT_ID` so the user can
/// match the app that issued their refresh token (a refresh token only redeems
/// against its own client id).
pub fn client_id() -> String {
    std::env::var("RECRAFT_MS_CLIENT_ID").unwrap_or_else(|_| DEFAULT_MS_CLIENT_ID.to_owned())
}

/// The Azure tenant segment of the endpoint URLs (`consumers` for personal
/// accounts, `common` for both), overridable via `RECRAFT_MS_TENANT`.
fn tenant() -> String {
    std::env::var("RECRAFT_MS_TENANT").unwrap_or_else(|_| "consumers".to_owned())
}

fn devicecode_url() -> String {
    format!(
        "https://login.microsoftonline.com/{}/oauth2/v2.0/devicecode",
        tenant()
    )
}

fn token_url() -> String {
    format!(
        "https://login.microsoftonline.com/{}/oauth2/v2.0/token",
        tenant()
    )
}

// Legacy MSA endpoints (login.live.com): used when the client id is a 16-hex
// legacy app such as the official launcher's `00000000402b5328`. These do NOT
// support the device-code grant, but DO support the refresh-token grant — which
// is how a refresh token taken from the official launcher is redeemed.
const LIVE_TOKEN_URL: &str = "https://login.live.com/oauth20_token.srf";
const LIVE_REDIRECT: &str = "https://login.live.com/oauth20_desktop.srf";
const LIVE_SCOPE: &str = "service::user.auth.xboxlive.com::MBI_SSL";

/// Whether the configured client id is a legacy 16-hex MSA app (e.g. the
/// official launcher's `00000000402b5328`). Those use login.live.com + the
/// MBI_SSL scope and a raw Xbox RpsTicket, not the v2.0 `d=`-prefixed one.
fn is_legacy_client() -> bool {
    let id = client_id();
    id.len() == 16 && id.chars().all(|c| c.is_ascii_hexdigit())
}

/// A fully authenticated Minecraft session.
#[derive(Debug, Clone)]
pub struct Session {
    /// Minecraft access token (from api.minecraftservices.com).
    pub access_token: String,
    /// UUID without dashes (as returned by the profile endpoint).
    pub uuid: String,
    /// In-game username.
    pub username: String,
    /// Latest (rotated) MS refresh token, if `offline_access` was granted.
    pub refresh_token: Option<String>,
}

/// Progress / result messages sent from a background auth thread to the UI.
#[derive(Debug, Clone)]
pub enum AuthEvent {
    /// Device-code flow initiated — show `verification_uri` + `user_code`.
    DeviceCode {
        user_code: String,
        verification_uri: String,
    },
    /// A progress message for the current auth step (Xbox Live, XSTS, …).
    Status(String),
    /// Authentication succeeded; the session carries the freshest refresh token.
    Success(Session),
    /// Authentication failed with a human-readable error.
    Failed(String),
}

/// Kick off the interactive device-code flow on a background thread.
pub fn start_login(tx: Sender<AuthEvent>) {
    std::thread::spawn(move || {
        if let Err(err) = run_device_code_flow(&tx) {
            let _ = tx.send(AuthEvent::Failed(err.to_string()));
        }
    });
}

/// Kick off a non-interactive login from a stored MS refresh token.
pub fn start_login_with_refresh_token(refresh_token: String, tx: Sender<AuthEvent>) {
    std::thread::spawn(move || match run_refresh_flow(&refresh_token, &tx) {
        Ok(session) => {
            let _ = tx.send(AuthEvent::Success(session));
        }
        Err(err) => {
            let _ = tx.send(AuthEvent::Failed(err.to_string()));
        }
    });
}

fn run_device_code_flow(tx: &Sender<AuthEvent>) -> Result<(), Box<dyn std::error::Error>> {
    // ── Step 1: request a device code ──────────────────────────────────────
    let device_resp = ureq::post(&devicecode_url())
        .set("Content-Type", "application/x-www-form-urlencoded")
        .send_string(&format!(
            "client_id={}&scope={}",
            client_id(),
            urlencoded(MS_SCOPES)
        ))?
        .into_json::<serde_json::Value>()?;

    let user_code = device_resp["user_code"]
        .as_str()
        .ok_or("missing user_code")?
        .to_owned();
    let device_code = device_resp["device_code"]
        .as_str()
        .ok_or("missing device_code")?
        .to_owned();
    let verification_uri = device_resp["verification_url"]
        .as_str()
        .or_else(|| device_resp["verification_uri"].as_str())
        .ok_or("missing verification_uri")?
        .to_owned();
    let interval_secs = device_resp["interval"].as_u64().unwrap_or(5);
    let expires_in_secs = device_resp["expires_in"].as_u64().unwrap_or(900);

    log::info!("MS login: go to {verification_uri} and enter code: {user_code}");
    let _ = tx.send(AuthEvent::DeviceCode {
        user_code,
        verification_uri,
    });

    // ── Step 2: poll for the MS access (+ refresh) token ───────────────────
    let (ms_token, refresh_token) =
        poll_device_token(&device_code, interval_secs, expires_in_secs)?;

    // ── Steps 3-6: Xbox → XSTS → Minecraft → profile ───────────────────────
    let session = complete_minecraft_auth(&ms_token, refresh_token, false, tx)?;
    let _ = tx.send(AuthEvent::Success(session));
    Ok(())
}

fn run_refresh_flow(
    refresh_token: &str,
    tx: &Sender<AuthEvent>,
) -> Result<Session, Box<dyn std::error::Error>> {
    let _ = tx.send(AuthEvent::Status("Refreshing Microsoft token…".to_owned()));
    let (ms_token, new_refresh) = redeem_refresh_token(refresh_token)?;
    complete_minecraft_auth(&ms_token, Some(new_refresh), is_legacy_client(), tx)
}

/// Redeem an MS refresh token for a fresh access token. Returns the access token
/// and the (possibly rotated) refresh token to persist — Microsoft rotates
/// refresh tokens, so we keep whatever the response gives, falling back to the
/// old one only if none is returned. Uses the legacy login.live.com endpoint for
/// legacy (official-launcher) client ids, else the v2.0 endpoint.
fn redeem_refresh_token(
    refresh_token: &str,
) -> Result<(String, String), Box<dyn std::error::Error>> {
    let resp = if is_legacy_client() {
        ureq::post(LIVE_TOKEN_URL)
            .set("Content-Type", "application/x-www-form-urlencoded")
            .send_string(&format!(
                "client_id={}&grant_type=refresh_token&refresh_token={}&redirect_uri={}&scope={}",
                client_id(),
                urlencoded(refresh_token),
                urlencoded(LIVE_REDIRECT),
                urlencoded(LIVE_SCOPE),
            ))
    } else {
        ureq::post(&token_url())
            .set("Content-Type", "application/x-www-form-urlencoded")
            .send_string(&format!(
                "client_id={}&grant_type=refresh_token&refresh_token={}&scope={}",
                client_id(),
                urlencoded(refresh_token),
                urlencoded(MS_SCOPES),
            ))
    };

    let body = match resp {
        Ok(r) => r.into_json::<serde_json::Value>()?,
        Err(ureq::Error::Status(_, r)) => {
            let body = r.into_json::<serde_json::Value>()?;
            let msg = body["error_description"]
                .as_str()
                .or_else(|| body["error"].as_str())
                .unwrap_or("refresh token rejected");
            return Err(format!("refresh failed: {msg}").into());
        }
        Err(e) => return Err(e.into()),
    };

    let access = body["access_token"]
        .as_str()
        .ok_or("refresh response missing access_token")?
        .to_owned();
    let new_refresh = body["refresh_token"]
        .as_str()
        .map(|s| s.to_owned())
        .unwrap_or_else(|| refresh_token.to_owned());
    Ok((access, new_refresh))
}

/// Run the Xbox Live → XSTS → Minecraft → profile chain from an MS access token,
/// returning the session (carrying `refresh_token` for persistence).
fn complete_minecraft_auth(
    ms_token: &str,
    refresh_token: Option<String>,
    legacy: bool,
    tx: &Sender<AuthEvent>,
) -> Result<Session, Box<dyn std::error::Error>> {
    // Xbox Live authenticate. Legacy (login.live.com / MBI_SSL) tokens use the
    // raw access token as the RpsTicket; v2.0 tokens use the `d=`-prefixed form.
    let _ = tx.send(AuthEvent::Status(
        "Authenticating with Xbox Live…".to_owned(),
    ));
    let rps_ticket = if legacy {
        ms_token.to_owned()
    } else {
        format!("d={ms_token}")
    };
    let xbl_resp = ureq::post("https://user.auth.xboxlive.com/user/authenticate")
        .set("Content-Type", "application/json")
        .set("Accept", "application/json")
        .send_json(serde_json::json!({
            "Properties": {
                "AuthMethod": "RPS",
                "SiteName": "user.auth.xboxlive.com",
                "RpsTicket": rps_ticket
            },
            "RelyingParty": "http://auth.xboxlive.com",
            "TokenType": "JWT"
        }))?
        .into_json::<serde_json::Value>()?;

    let xbl_token = xbl_resp["Token"]
        .as_str()
        .ok_or("missing XBL token")?
        .to_owned();
    let uhs = xbl_resp["DisplayClaims"]["xui"]
        .as_array()
        .and_then(|arr| arr.first())
        .and_then(|entry| entry["uhs"].as_str())
        .ok_or("missing XBL uhs")?
        .to_owned();

    // XSTS token.
    let _ = tx.send(AuthEvent::Status("Authorizing (XSTS)…".to_owned()));
    let xsts_resp = ureq::post("https://xsts.auth.xboxlive.com/xsts/authorize")
        .set("Content-Type", "application/json")
        .set("Accept", "application/json")
        .send_json(serde_json::json!({
            "Properties": { "SandboxId": "RETAIL", "UserTokens": [xbl_token] },
            "RelyingParty": "rp://api.minecraftservices.com/",
            "TokenType": "JWT"
        }))?
        .into_json::<serde_json::Value>()?;

    if let Some(err_code) = xsts_resp["XErr"].as_u64() {
        let msg = match err_code {
            2148916233 => "Xbox account not found — create one at xbox.com",
            2148916235 => "Xbox Live is unavailable in your region",
            2148916236 | 2148916237 => "Adult consent required",
            2148916238 => "Under-age account requires parental consent",
            _ => "XSTS authorization failed",
        };
        return Err(format!("{msg} (XErr {err_code})").into());
    }

    let xsts_token = xsts_resp["Token"]
        .as_str()
        .ok_or("missing XSTS token")?
        .to_owned();

    // Minecraft access token.
    let _ = tx.send(AuthEvent::Status("Getting Minecraft token…".to_owned()));
    let identity_token = format!("XBL3.0 x={uhs};{xsts_token}");
    let mc_resp = ureq::post("https://api.minecraftservices.com/authentication/login_with_xbox")
        .set("Content-Type", "application/json")
        .send_json(serde_json::json!({ "identityToken": identity_token }))?
        .into_json::<serde_json::Value>()?;

    let mc_token = mc_resp["access_token"]
        .as_str()
        .ok_or("missing Minecraft access token")?
        .to_owned();

    // Profile (uuid + username).
    let _ = tx.send(AuthEvent::Status("Loading Minecraft profile…".to_owned()));
    let profile = ureq::get("https://api.minecraftservices.com/minecraft/profile")
        .set("Authorization", &format!("Bearer {mc_token}"))
        .call()?
        .into_json::<serde_json::Value>()?;

    let uuid = profile["id"]
        .as_str()
        .ok_or("missing profile id (does this account own Minecraft?)")?
        .to_owned();
    let username = profile["name"]
        .as_str()
        .ok_or("missing profile name")?
        .to_owned();

    log::info!("MS auth success: {username} ({uuid})");
    Ok(Session {
        access_token: mc_token,
        uuid,
        username,
        refresh_token,
    })
}

/// Poll the token endpoint until the user completes the device-code flow or it
/// expires. Returns the MS access token and the refresh token.
fn poll_device_token(
    device_code: &str,
    interval_secs: u64,
    expires_in_secs: u64,
) -> Result<(String, Option<String>), Box<dyn std::error::Error>> {
    let deadline = Instant::now() + Duration::from_secs(expires_in_secs);
    let poll_interval = Duration::from_secs(interval_secs.max(5));

    loop {
        std::thread::sleep(poll_interval);
        if Instant::now() > deadline {
            return Err("device code expired before login completed".into());
        }

        let resp = ureq::post(&token_url())
            .set("Content-Type", "application/x-www-form-urlencoded")
            .send_string(&format!(
                "client_id={}&grant_type=urn:ietf:params:oauth:grant-type:device_code&device_code={}",
                client_id(), device_code
            ));

        let body = match resp {
            Ok(r) => r.into_json::<serde_json::Value>()?,
            // 400 carries the pending/slow_down/error json.
            Err(ureq::Error::Status(400, r)) => r.into_json::<serde_json::Value>()?,
            Err(e) => return Err(e.into()),
        };

        if let Some(token) = body["access_token"].as_str() {
            let refresh = body["refresh_token"].as_str().map(|s| s.to_owned());
            return Ok((token.to_owned(), refresh));
        }
        let err_code = body["error"].as_str().unwrap_or("unknown");
        if err_code != "authorization_pending" && err_code != "slow_down" {
            return Err(format!(
                "token error: {}",
                body["error_description"].as_str().unwrap_or(err_code)
            )
            .into());
        }
    }
}

// ─── Persistent account storage ──────────────────────────────────────────────

/// A stored account: just enough to silently re-login via its refresh token.
#[derive(Debug, Clone)]
pub struct Account {
    pub username: String,
    pub uuid: String,
    pub refresh_token: String,
}

/// The set of saved accounts, persisted as JSON.
#[derive(Debug, Clone, Default)]
pub struct AccountStore {
    pub accounts: Vec<Account>,
}

impl AccountStore {
    /// Load saved accounts, or an empty store if the file is missing/unreadable.
    pub fn load() -> Self {
        let Ok(text) = std::fs::read_to_string(store_path()) else {
            return Self::default();
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
            return Self::default();
        };
        let accounts = value
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|entry| {
                        Some(Account {
                            username: entry["username"].as_str()?.to_owned(),
                            uuid: entry["uuid"].as_str()?.to_owned(),
                            refresh_token: entry["refresh_token"].as_str()?.to_owned(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self { accounts }
    }

    /// Persist the store to disk (best effort; logs on failure).
    pub fn save(&self) {
        let array: Vec<serde_json::Value> = self
            .accounts
            .iter()
            .map(|a| {
                serde_json::json!({
                    "username": a.username,
                    "uuid": a.uuid,
                    "refresh_token": a.refresh_token,
                })
            })
            .collect();
        let path = store_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match serde_json::to_string_pretty(&serde_json::Value::Array(array)) {
            Ok(text) => {
                if let Err(err) = std::fs::write(&path, text) {
                    log::warn!("failed to save accounts to {}: {err}", path.display());
                }
            }
            Err(err) => log::warn!("failed to serialize accounts: {err}"),
        }
    }

    /// Insert or replace an account (matched by uuid), keeping the newest token.
    pub fn upsert(&mut self, account: Account) {
        if let Some(existing) = self.accounts.iter_mut().find(|a| a.uuid == account.uuid) {
            *existing = account;
        } else {
            self.accounts.push(account);
        }
    }

    /// Remove an account by uuid.
    pub fn remove(&mut self, uuid: &str) {
        self.accounts.retain(|a| a.uuid != uuid);
    }

    /// Record a freshly authenticated session (saving its rotated refresh token).
    pub fn record_session(&mut self, session: &Session) {
        if let Some(refresh_token) = &session.refresh_token {
            self.upsert(Account {
                username: session.username.clone(),
                uuid: session.uuid.clone(),
                refresh_token: refresh_token.clone(),
            });
            self.save();
        }
    }
}

/// The recraft config directory (`<config dir>/recraft/`), shared by the
/// accounts and server-list stores.
pub fn config_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("APPDATA").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("recraft")
}

/// Path of the accounts file: `<config dir>/recraft/accounts.json`.
fn store_path() -> PathBuf {
    config_dir().join("accounts.json")
}

/// Percent-encode a string for `application/x-www-form-urlencoded` bodies.
fn urlencoded(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => out.push(c),
            ' ' => out.push_str("%20"),
            c => {
                let mut buf = [0u8; 4];
                for byte in c.encode_utf8(&mut buf).bytes() {
                    out.push('%');
                    out.push(hex_nibble(byte >> 4));
                    out.push(hex_nibble(byte & 0xf));
                }
            }
        }
    }
    out
}

fn hex_nibble(n: u8) -> char {
    match n {
        0..=9 => (b'0' + n) as char,
        _ => (b'a' + n - 10) as char,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_store_round_trips_via_json() {
        // Build a store, serialize the JSON the way save() does, and re-parse it
        // the way load() does — verifying the on-disk shape round-trips.
        let store = AccountStore {
            accounts: vec![Account {
                username: "Steve".into(),
                uuid: "abcd".into(),
                refresh_token: "rt-123".into(),
            }],
        };
        let array: Vec<serde_json::Value> = store
            .accounts
            .iter()
            .map(|a| {
                serde_json::json!({
                    "username": a.username, "uuid": a.uuid, "refresh_token": a.refresh_token,
                })
            })
            .collect();
        let text = serde_json::to_string(&serde_json::Value::Array(array)).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        let back = &value.as_array().unwrap()[0];
        assert_eq!(back["username"], "Steve");
        assert_eq!(back["refresh_token"], "rt-123");
    }

    #[test]
    fn upsert_replaces_by_uuid() {
        let mut store = AccountStore::default();
        store.upsert(Account {
            username: "Steve".into(),
            uuid: "u1".into(),
            refresh_token: "old".into(),
        });
        store.upsert(Account {
            username: "Steve".into(),
            uuid: "u1".into(),
            refresh_token: "new".into(),
        });
        assert_eq!(store.accounts.len(), 1);
        assert_eq!(store.accounts[0].refresh_token, "new");
    }

    #[test]
    fn urlencoding_encodes_reserved_chars() {
        // Percent-encoding hex is lowercase here (case-insensitive per RFC 3986).
        assert_eq!(urlencoded("a b+c/d"), "a%20b%2bc%2fd");
    }
}
