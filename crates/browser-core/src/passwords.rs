//! Saved sign-ins: encryption at rest, the CSV other password managers read and write, a generator and health checks.

use aes_gcm::aead::rand_core::RngCore;
use aes_gcm::aead::{Aead, AeadCore, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use anyhow::Context;
use std::collections::HashMap;

/// AES-256 key; the platform keeps it outside the database (the macOS Keychain).
pub type VaultKey = [u8; 32];

#[derive(Clone, Debug, PartialEq)]
pub struct Login {
    pub id: String,
    /// `scheme://host[:port]`, the page origin the password was saved on.
    pub origin: String,
    pub username: String,
    pub note: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub last_used_at: i64,
}

/// One row from another manager's export.
#[derive(Clone, Debug, PartialEq)]
pub struct CsvLogin {
    pub url: String,
    pub username: String,
    pub password: String,
    pub note: String,
}

const NONCE_LEN: usize = 12;

/// 12-byte random nonce followed by the AES-GCM ciphertext and tag.
pub fn encrypt(key: &VaultKey, plain: &str) -> anyhow::Result<Vec<u8>> {
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let sealed = cipher.encrypt(&nonce, plain.as_bytes()).map_err(|_| anyhow::anyhow!("encrypt failed"))?;
    Ok([nonce.as_slice(), &sealed].concat())
}

pub fn decrypt(key: &VaultKey, blob: &[u8]) -> anyhow::Result<String> {
    anyhow::ensure!(blob.len() > NONCE_LEN, "password record too short");
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let (nonce, sealed) = blob.split_at(NONCE_LEN);
    let plain = cipher
        .decrypt(Nonce::from_slice(nonce), sealed)
        .map_err(|_| anyhow::anyhow!("password does not decrypt with this key"))?;
    String::from_utf8(plain).context("password is not UTF-8")
}

/// `scheme://host[:port]` of an http(s) URL, lowercased, default port dropped; `None` for anything else.
pub fn origin_of(url: &str) -> Option<String> {
    let url = url.trim();
    let (scheme, rest) = url.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next()?;
    let host_port = authority.rsplit('@').next()?.to_ascii_lowercase();
    let default = if scheme == "https" { ":443" } else { ":80" };
    let host_port = host_port.strip_suffix(default).unwrap_or(&host_port);
    (!host_port.is_empty()).then(|| format!("{scheme}://{host_port}"))
}

/// Host shown for an origin: `https://www.example.com` → `www.example.com`.
pub fn site_of(origin: &str) -> &str {
    origin.split_once("://").map_or(origin, |(_, host)| host)
}

const LOWER: &[u8] = b"abcdefghijkmnpqrstuvwxyz";
const UPPER: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ";
const DIGITS: &[u8] = b"23456789";
const SYMBOLS: &[u8] = b"-_.!@#$%*";

/// 20 characters with every class present and look-alikes (l, 1, I, O, 0) left out.
pub fn generate() -> String {
    let all: Vec<u8> = [LOWER, UPPER, DIGITS, SYMBOLS].concat();
    loop {
        // Modulo bias over a u32 is ~1e-8 for a 65-char alphabet: not worth rejection sampling.
        let password: String = (0..20).map(|_| all[OsRng.next_u32() as usize % all.len()] as char).collect();
        if [LOWER, UPPER, DIGITS, SYMBOLS].iter().all(|class| password.bytes().any(|b| class.contains(&b))) {
            return password;
        }
    }
}

const COMMON: [&str; 20] = [
    "password", "password1", "password123", "123456", "12345678", "123456789", "1234567890", "qwerty",
    "qwerty123", "abc123", "111111", "letmein", "welcome", "iloveyou", "admin", "monkey", "dragon",
    "football", "sunshine", "passw0rd",
];

pub fn is_weak(password: &str) -> bool {
    let classes = [
        password.chars().any(|c| c.is_ascii_lowercase()),
        password.chars().any(|c| c.is_ascii_uppercase()),
        password.chars().any(|c| c.is_ascii_digit()),
        password.chars().any(|c| !c.is_ascii_alphanumeric()),
    ];
    password.chars().count() < 8
        || classes.iter().filter(|&&has| has).count() <= 1
        || COMMON.contains(&password.to_lowercase().as_str())
}

/// Ids of logins whose password is also used by another saved login.
pub fn reused(rows: &[(Login, String)]) -> Vec<String> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for (_, password) in rows {
        *counts.entry(password).or_default() += 1;
    }
    rows.iter().filter(|(_, password)| counts[password.as_str()] > 1).map(|(login, _)| login.id.clone()).collect()
}

/// Chrome's export format (`name,url,username,password,note`), which Google, Apple Passwords,
/// Firefox, Bitwarden and 1Password all import.
pub fn to_csv(rows: &[(Login, String)]) -> String {
    let mut out = String::from("name,url,username,password,note\n");
    for (login, password) in rows {
        let fields = [site_of(&login.origin), &login.origin, &login.username, password, &login.note];
        out.push_str(&fields.iter().map(|field| csv_field(field)).collect::<Vec<_>>().join(","));
        out.push('\n');
    }
    out
}

fn csv_field(field: &str) -> String {
    if field.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}

/// Reads exports from Chrome, Safari/Apple Passwords, Firefox, Bitwarden and 1Password by their headers.
pub fn from_csv(text: &str) -> anyhow::Result<Vec<CsvLogin>> {
    let mut records = parse_csv(text.trim_start_matches('\u{feff}')).into_iter();
    let header: Vec<String> = records.next().context("empty CSV")?.iter().map(|h| h.trim().to_lowercase()).collect();
    let column = |names: &[&str]| header.iter().position(|h| names.contains(&h.as_str()));
    let url = column(&["url", "login_uri", "website"]).context("CSV has no url column")?;
    let password = column(&["password", "login_password"]).context("CSV has no password column")?;
    let username = column(&["username", "login_username", "user"]);
    let note = column(&["note", "notes", "extra", "comments"]);
    let get = |record: &[String], index: Option<usize>| index.and_then(|i| record.get(i)).cloned().unwrap_or_default();
    Ok(records
        .filter(|record| record.iter().any(|field| !field.is_empty()))
        .map(|record| CsvLogin {
            url: get(&record, Some(url)),
            username: get(&record, username),
            password: get(&record, Some(password)),
            note: get(&record, note),
        })
        .collect())
}

/// RFC 4180: quoted fields may hold commas, doubled quotes and line breaks.
fn parse_csv(text: &str) -> Vec<Vec<String>> {
    let mut records = Vec::new();
    let mut record = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match (quoted, c) {
            (true, '"') if chars.peek() == Some(&'"') => {
                chars.next();
                field.push('"');
            }
            (true, '"') => quoted = false,
            (true, c) => field.push(c),
            (false, '"') if field.is_empty() => quoted = true,
            (false, ',') => record.push(std::mem::take(&mut field)),
            (false, '\r') if chars.peek() == Some(&'\n') => {}
            (false, '\n' | '\r') => {
                record.push(std::mem::take(&mut field));
                records.push(std::mem::take(&mut record));
            }
            (false, c) => field.push(c),
        }
    }
    if !field.is_empty() || !record.is_empty() {
        record.push(field);
        records.push(record);
    }
    records
}

#[cfg(test)]
mod tests {
    use super::*;

    fn login(id: &str, origin: &str, username: &str) -> Login {
        Login {
            id: id.into(),
            origin: origin.into(),
            username: username.into(),
            note: String::new(),
            created_at: 0,
            updated_at: 0,
            last_used_at: 0,
        }
    }

    #[test]
    fn encrypt_round_trips_and_rejects_other_keys() {
        let key = [7u8; 32];
        let blob = encrypt(&key, "hunter2 ✓").unwrap();
        assert_eq!(decrypt(&key, &blob).unwrap(), "hunter2 ✓");
        assert_ne!(encrypt(&key, "hunter2 ✓").unwrap(), blob, "fresh nonce each time");
        assert!(decrypt(&[8u8; 32], &blob).is_err());
        assert!(decrypt(&key, &blob[..5]).is_err());
    }

    #[test]
    fn origins_are_normalized() {
        assert_eq!(origin_of("https://Example.com:443/login?x=1").as_deref(), Some("https://example.com"));
        assert_eq!(origin_of("http://user:pw@host:8080#a").as_deref(), Some("http://host:8080"));
        assert_eq!(origin_of("https://example.com").as_deref(), Some("https://example.com"));
        assert_eq!(origin_of("android://abc@com.app/"), None);
        assert_eq!(origin_of("example.com"), None);
        assert_eq!(site_of("https://www.example.com"), "www.example.com");
    }

    #[test]
    fn generated_passwords_are_strong() {
        for _ in 0..50 {
            let password = generate();
            assert_eq!(password.len(), 20);
            assert!(!is_weak(&password));
            assert!(!password.contains(['l', '1', 'I', 'O', '0']));
        }
    }

    #[test]
    fn weak_and_reused() {
        assert!(is_weak("short1A"));
        assert!(is_weak("alllowercaseletters"));
        assert!(is_weak("Password1"), "common once lowercased");
        assert!(!is_weak("Tr0ub4dor"));
        assert!(is_weak("Passw0rd"));
        let rows = vec![
            (login("a", "https://a.com", "x"), "same".to_string()),
            (login("b", "https://b.com", "y"), "same".to_string()),
            (login("c", "https://c.com", "z"), "other".to_string()),
        ];
        assert_eq!(reused(&rows), vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn csv_round_trips_awkward_fields() {
        let mut tricky = login("a", "https://example.com", "me, \"quoted\"");
        tricky.note = "line one\nline two".into();
        let csv = to_csv(&[(tricky, "p,a\"ss\r\nword".into())]);
        assert!(csv.starts_with("name,url,username,password,note\n"));
        let rows = from_csv(&csv).unwrap();
        assert_eq!(
            rows,
            vec![CsvLogin {
                url: "https://example.com".into(),
                username: "me, \"quoted\"".into(),
                password: "p,a\"ss\r\nword".into(),
                note: "line one\nline two".into(),
            }]
        );
    }

    #[test]
    fn csv_reads_other_managers() {
        let apple = "\u{feff}Title,URL,Username,Password,Notes,OTPAuth\r\nSite,https://a.com/,ann,pw1,hi,\r\n";
        let firefox = "\"url\",\"username\",\"password\",\"httpRealm\"\n\"https://b.com\",\"bob\",\"pw2\",\"\"\n\n";
        let bitwarden = "folder,name,login_uri,login_username,login_password,notes\n,B,https://c.com,cat,pw3,\n";
        assert_eq!(from_csv(apple).unwrap()[0], CsvLogin { url: "https://a.com/".into(), username: "ann".into(), password: "pw1".into(), note: "hi".into() });
        assert_eq!(from_csv(firefox).unwrap().len(), 1);
        assert_eq!(from_csv(firefox).unwrap()[0].username, "bob");
        assert_eq!(from_csv(bitwarden).unwrap()[0].password, "pw3");
        assert!(from_csv("a,b\n1,2").is_err());
    }
}
