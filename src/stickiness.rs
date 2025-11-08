use crate::errors::RouterError;
use base64::Engine as _;
use chrono::{DateTime, Duration, Utc};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use uuid::Uuid;

pub type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StickinessClaims {
    pub token_id: String,
    pub tenant: Option<String>,
    pub project: Option<String>,
    pub alias: String,
    pub model_id: String,
    pub expires_at: DateTime<Utc>,
    pub max_turns: u8,
    pub turn: u8,
}

#[derive(Clone)]
pub struct StickinessManager {
    secret: Vec<u8>,
    engine: base64::engine::general_purpose::GeneralPurpose,
}

impl StickinessManager {
    pub fn new(secret: Vec<u8>) -> Self {
        Self {
            secret,
            engine: base64::engine::general_purpose::URL_SAFE_NO_PAD,
        }
    }

    pub fn issue(
        &self,
        tenant: Option<&str>,
        project: Option<&str>,
        alias: &str,
        model_id: &str,
        max_turns: u8,
        ttl_ms: u64,
    ) -> Result<(String, StickinessClaims), RouterError> {
        let expires_at = Utc::now() + Duration::milliseconds(ttl_ms as i64);
        let claims = StickinessClaims {
            token_id: Uuid::new_v4().to_string(),
            tenant: tenant.map(|s| s.to_string()),
            project: project.map(|s| s.to_string()),
            alias: alias.to_string(),
            model_id: model_id.to_string(),
            expires_at,
            max_turns,
            turn: 0,
        };
        let token = self.sign_claims(claims.clone())?;
        Ok((token, claims))
    }

    pub fn progress_turn(
        &self,
        claims: &StickinessClaims,
        ttl_ms: u64,
    ) -> Result<(String, StickinessClaims), RouterError> {
        let expires_at = Utc::now() + Duration::milliseconds(ttl_ms as i64);
        let mut next = claims.clone();
        next.turn = next.turn.saturating_add(1);
        next.expires_at = expires_at;
        next.token_id = Uuid::new_v4().to_string();
        let token = self.sign_claims(next.clone())?;
        Ok((token, next))
    }

    pub fn verify(&self, token: &str) -> Result<StickinessClaims, RouterError> {
        let raw = self
            .engine
            .decode(token)
            .map_err(|err| RouterError::InvalidApproval(format!("bad token encoding: {err}")))?;
        if raw.len() < 32 {
            return Err(RouterError::InvalidApproval("token too short".into()));
        }
        let (payload, sig) = raw.split_at(raw.len() - 32);

        let mut mac = HmacSha256::new_from_slice(&self.secret)
            .map_err(|_| RouterError::InvalidApproval("invalid secret".into()))?;
        mac.update(payload);
        mac.verify_slice(sig)
            .map_err(|_| RouterError::InvalidApproval("signature mismatch".into()))?;

        let claims: StickinessClaims = serde_json::from_slice(payload)
            .map_err(|err| RouterError::InvalidApproval(format!("invalid claims: {err}")))?;

        if claims.expires_at < Utc::now() {
            return Err(RouterError::InvalidApproval("token expired".into()));
        }

        Ok(claims)
    }

    fn sign_claims(&self, claims: StickinessClaims) -> Result<String, RouterError> {
        let payload = serde_json::to_vec(&claims)
            .map_err(|err| RouterError::InvalidApproval(format!("serialize claims: {err}")))?;
        let mut mac = HmacSha256::new_from_slice(&self.secret)
            .map_err(|_| RouterError::InvalidApproval("invalid secret".into()))?;
        mac.update(&payload);
        let sig = mac.finalize().into_bytes();
        let mut out = payload;
        out.extend_from_slice(&sig);
        Ok(self.engine.encode(out))
    }
}
