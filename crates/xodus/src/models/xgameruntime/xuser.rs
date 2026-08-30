use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct MSATokenRequest {
    pub client_id: String,
    #[serde(default)]
    pub allow_ui: bool,
    #[serde(default, alias = "MSAFullTrust")]
    pub msa_full_trust: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct MSATokenResponse {
    pub token: String,
    pub expiry: i64,
    pub device_rps: String,
    pub device_expiry: i64,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ErrorResponse {
    pub message: String,
}

/// Identity of the signed-in user, resolved from the XSTS display claims.
/// Fields whose claim is absent serialize as empty strings.
#[derive(Serialize, Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
pub struct UserIdentityResponse {
    pub xuid: String,
    pub gamertag: String,
    pub modern_gamertag: String,
    pub user_hash: String,
    /// Unix timestamp after which the claims should be re-requested.
    pub expiry: i64,
}

#[cfg(test)]
mod tests {
    use super::UserIdentityResponse;

    #[test]
    fn user_identity_response_round_trips_through_xml() {
        let original = UserIdentityResponse {
            xuid: "2814921234567890".to_string(),
            gamertag: "Xodus Tester".to_string(),
            modern_gamertag: "XodusTester".to_string(),
            user_hash: "1122334455667788".to_string(),
            expiry: 1_800_000_000,
        };
        let xml = quick_xml::se::to_string(&original).unwrap();
        let parsed: UserIdentityResponse = quick_xml::de::from_str(&xml).unwrap();
        assert_eq!(parsed.xuid, original.xuid);
        assert_eq!(parsed.gamertag, original.gamertag);
        assert_eq!(parsed.modern_gamertag, original.modern_gamertag);
        assert_eq!(parsed.user_hash, original.user_hash);
        assert_eq!(parsed.expiry, original.expiry);
    }
}
