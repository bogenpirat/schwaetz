//! SASL mechanisms (<https://ircv3.net/specs/extensions/sasl-3.2>).

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

/// Credentials configured for a network.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SaslConfig {
    Plain {
        username: String,
        password: String,
    },
    /// Authenticate with the TLS client certificate (CertFP).
    External,
    ScramSha256 {
        username: String,
        password: String,
    },
}

impl SaslConfig {
    pub fn mechanism(&self) -> &'static str {
        match self {
            SaslConfig::Plain { .. } => "PLAIN",
            SaslConfig::External => "EXTERNAL",
            SaslConfig::ScramSha256 { .. } => "SCRAM-SHA-256",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum SaslError {
    /// The server's challenge could not be parsed or failed verification.
    BadServerMessage(&'static str),
}

/// A running SASL exchange. Feed decoded server challenges, get raw client responses.
pub struct SaslSession {
    state: State,
}

enum State {
    Plain { username: String, password: String, sent: bool },
    External { sent: bool },
    Scram(Scram),
    Done,
}

impl SaslSession {
    pub fn new(cfg: &SaslConfig) -> SaslSession {
        let state = match cfg {
            SaslConfig::Plain { username, password } => {
                State::Plain { username: username.clone(), password: password.clone(), sent: false }
            }
            SaslConfig::External => State::External { sent: false },
            SaslConfig::ScramSha256 { username, password } => State::Scram(Scram::new(username, password, None)),
        };
        SaslSession { state }
    }

    #[cfg(test)]
    fn with_scram_nonce(username: &str, password: &str, nonce: &str) -> SaslSession {
        SaslSession { state: State::Scram(Scram::new(username, password, Some(nonce.to_owned()))) }
    }

    /// Returns the next client response (raw, not base64) for a server challenge.
    /// `Ok(None)` means the exchange is complete from the client's side.
    pub fn step(&mut self, challenge: &[u8]) -> Result<Option<Vec<u8>>, SaslError> {
        match &mut self.state {
            State::Plain { username, password, sent } => {
                if *sent {
                    return Ok(None);
                }
                *sent = true;
                // authzid is left empty: the server derives it from authcid.
                let mut out = Vec::with_capacity(username.len() + password.len() + 2);
                out.push(0);
                out.extend_from_slice(username.as_bytes());
                out.push(0);
                out.extend_from_slice(password.as_bytes());
                Ok(Some(out))
            }
            State::External { sent } => {
                if *sent {
                    return Ok(None);
                }
                *sent = true;
                Ok(Some(Vec::new()))
            }
            State::Scram(scram) => {
                let r = scram.step(challenge);
                if matches!(r, Ok(None)) {
                    self.state = State::Done;
                }
                r
            }
            State::Done => Ok(None),
        }
    }
}

type HmacSha256 = Hmac<Sha256>;

struct Scram {
    username: String,
    password: String,
    nonce: String,
    stage: ScramStage,
}

enum ScramStage {
    Initial,
    SentFirst { client_first_bare: String },
    SentFinal { server_signature: Vec<u8> },
    Verified,
}

impl Scram {
    fn new(username: &str, password: &str, nonce: Option<String>) -> Scram {
        let nonce = nonce.unwrap_or_else(|| {
            let mut raw = [0u8; 24];
            // Falls back to a time-derived nonce only if the OS RNG is unavailable.
            if getrandom::fill(&mut raw).is_err() {
                let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
                raw[..16].copy_from_slice(&t.as_nanos().to_le_bytes());
            }
            B64.encode(raw)
        });
        Scram { username: username.to_owned(), password: password.to_owned(), nonce, stage: ScramStage::Initial }
    }

    fn step(&mut self, challenge: &[u8]) -> Result<Option<Vec<u8>>, SaslError> {
        match std::mem::replace(&mut self.stage, ScramStage::Verified) {
            ScramStage::Initial => {
                let bare = format!("n={},r={}", saslname(&self.username), self.nonce);
                let first = format!("n,,{bare}");
                self.stage = ScramStage::SentFirst { client_first_bare: bare };
                Ok(Some(first.into_bytes()))
            }
            ScramStage::SentFirst { client_first_bare } => {
                let server_first =
                    std::str::from_utf8(challenge).map_err(|_| SaslError::BadServerMessage("not utf-8"))?;
                let (mut r, mut s, mut i) = (None, None, None);
                for attr in server_first.split(',') {
                    match attr.split_at_checked(2) {
                        Some(("r=", v)) => r = Some(v),
                        Some(("s=", v)) => s = Some(v),
                        Some(("i=", v)) => i = v.parse::<u32>().ok(),
                        Some(("m=", _)) => return Err(SaslError::BadServerMessage("unsupported extension")),
                        _ => {}
                    }
                }
                let (Some(r), Some(s), Some(i)) = (r, s, i) else {
                    return Err(SaslError::BadServerMessage("missing attributes"));
                };
                if !r.starts_with(&self.nonce) || r.len() == self.nonce.len() {
                    return Err(SaslError::BadServerMessage("nonce mismatch"));
                }
                if i == 0 || i > 1_000_000 {
                    return Err(SaslError::BadServerMessage("bad iteration count"));
                }
                let salt = B64.decode(s).map_err(|_| SaslError::BadServerMessage("bad salt"))?;
                let mut salted = [0u8; 32];
                pbkdf2::pbkdf2_hmac::<Sha256>(self.password.as_bytes(), &salt, i, &mut salted);
                let client_key = hmac(&salted, b"Client Key");
                let stored_key = Sha256::digest(&client_key);
                let without_proof = format!("c=biws,r={r}");
                let auth_message = format!("{client_first_bare},{server_first},{without_proof}");
                let client_sig = hmac(&stored_key, auth_message.as_bytes());
                let proof: Vec<u8> = client_key.iter().zip(&client_sig).map(|(a, b)| a ^ b).collect();
                let server_key = hmac(&salted, b"Server Key");
                let server_signature = hmac(&server_key, auth_message.as_bytes());
                self.stage = ScramStage::SentFinal { server_signature };
                Ok(Some(format!("{without_proof},p={}", B64.encode(proof)).into_bytes()))
            }
            ScramStage::SentFinal { server_signature } => {
                let server_final =
                    std::str::from_utf8(challenge).map_err(|_| SaslError::BadServerMessage("not utf-8"))?;
                if server_final.starts_with("e=") {
                    return Err(SaslError::BadServerMessage("server reported error"));
                }
                let v = server_final
                    .split(',')
                    .find_map(|a| a.strip_prefix("v="))
                    .ok_or(SaslError::BadServerMessage("missing verifier"))?;
                let v = B64.decode(v).map_err(|_| SaslError::BadServerMessage("bad verifier"))?;
                if v != server_signature {
                    return Err(SaslError::BadServerMessage("server signature mismatch"));
                }
                // Some servers expect an empty final response; the caller sends "+".
                Ok(Some(Vec::new()))
            }
            ScramStage::Verified => Ok(None),
        }
    }
}

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("hmac accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn saslname(name: &str) -> String {
    name.replace('=', "=3D").replace(',', "=2C")
}

/// Encodes a client response into `AUTHENTICATE` parameters (400-byte base64 chunks, `+` for empty
/// or to terminate an exact multiple of 400).
pub fn encode_response(raw: &[u8]) -> Vec<String> {
    if raw.is_empty() {
        return vec!["+".into()];
    }
    let enc = B64.encode(raw);
    let mut out: Vec<String> = enc.as_bytes().chunks(400).map(|c| String::from_utf8_lossy(c).into_owned()).collect();
    if enc.len().is_multiple_of(400) {
        out.push("+".into());
    }
    out
}

/// Accumulates `AUTHENTICATE` chunks from the server.
#[derive(Default)]
pub struct ChallengeBuffer {
    buf: String,
}

impl ChallengeBuffer {
    /// Returns the complete decoded challenge once the final chunk has arrived.
    pub fn push(&mut self, chunk: &str) -> Option<Vec<u8>> {
        if chunk != "+" {
            self.buf.push_str(chunk);
        }
        if chunk.len() == 400 {
            return None;
        }
        let data = std::mem::take(&mut self.buf);
        Some(B64.decode(data.as_bytes()).unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain() {
        let mut s = SaslSession::new(&SaslConfig::Plain { username: "jilles".into(), password: "sesame".into() });
        assert_eq!(s.step(b"").unwrap().unwrap(), b"\0jilles\0sesame");
        assert_eq!(encode_response(b"\0jilles\0sesame"), vec!["AGppbGxlcwBzZXNhbWU="]);
    }

    #[test]
    fn scram_rfc7677() {
        let mut s = SaslSession::with_scram_nonce("user", "pencil", "rOprNGfwEbeRWgbNEkqO");
        let first = s.step(b"").unwrap().unwrap();
        assert_eq!(first, b"n,,n=user,r=rOprNGfwEbeRWgbNEkqO");
        let server_first = b"r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096";
        let fin = s.step(server_first).unwrap().unwrap();
        assert_eq!(
            String::from_utf8(fin).unwrap(),
            "c=biws,r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,p=dHzbZapWIk4jUhN+Ute9ytag9zjfMHgsqmmiz7AndVQ="
        );
        assert_eq!(s.step(b"v=6rriTRBi23WpRR/wtup+mMhUZUn/dB5nLTJRsjl95G4=").unwrap(), Some(vec![]));
    }

    #[test]
    fn scram_rejects_bad_signature() {
        let mut s = SaslSession::with_scram_nonce("user", "pencil", "rOprNGfwEbeRWgbNEkqO");
        s.step(b"").unwrap();
        s.step(b"r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096").unwrap();
        assert!(s.step(b"v=AAAA").is_err());
    }

    #[test]
    fn chunking() {
        let raw = vec![b'x'; 300]; // 400 base64 chars exactly
        let parts = encode_response(&raw);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[1], "+");
        let mut cb = ChallengeBuffer::default();
        assert_eq!(cb.push(&parts[0]), None);
        assert_eq!(cb.push(&parts[1]).unwrap(), raw);
    }
}
