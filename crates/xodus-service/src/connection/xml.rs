use tokio::io::{AsyncReadExt, AsyncWriteExt};
use xodus::ipc::{XML_MAGIC, encode_frame};
use xodus::models::live::ExchangeUserTokenOutcome;
use xodus::models::secrets::Token;
use xodus::models::soap;
use xodus::models::xgameruntime::xuser::{
    ErrorResponse, MSATokenRequest, MSATokenResponse, UserIdentityResponse,
};
use xodus::proto::xodus::XodusMessageType;

use crate::simple_context::SimpleContext;

pub async fn handle(
    socket: &mut tokio::net::UnixStream,
    context: &mut SimpleContext,
) -> tokio::io::Result<()> {
    log::debug!("Parsing XML");
    let message_type = socket.read_u16_le().await?;
    let message_size = socket.read_u16_le().await?;
    let mut buffer = vec![0; message_size as usize];
    log::debug!("Reading buffer {message_size}");
    socket.read_exact(&mut buffer).await?;
    log::debug!("Read buffer");
    let message_type = XodusMessageType::try_from(message_type as i32).unwrap_or_default();

    // A failed request answers with a typed ERROR_RESPONSE frame instead of an
    // empty <request + 1> payload, so clients can tell "failed" from "empty".
    let data = match parse_message(context, message_type, buffer).await {
        Ok(buf) => encode_frame(XML_MAGIC, message_type as u16 + 1, &buf),
        Err(err) => {
            log::error!("Failed handling {message_type:?}: {err}");
            let payload = quick_xml::se::to_string(&ErrorResponse {
                message: err.to_string(),
            })
            .unwrap_or_default();
            encode_frame(
                XML_MAGIC,
                XodusMessageType::ErrorResponse as u16,
                payload.as_bytes(),
            )
        }
    };
    socket.write_all(&data).await
}

pub async fn parse_message(
    context: &mut SimpleContext,
    message_type: XodusMessageType,
    buffer: Vec<u8>,
) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    match message_type {
        XodusMessageType::Ping => Ok(buffer),
        XodusMessageType::MsaTokenRequest => {
            log::debug!("Raw buffer: {buffer:?}");
            let string_buf = std::str::from_utf8(&buffer)?;
            log::debug!("String buffer: {string_buf:?}");
            let req = quick_xml::de::from_str::<MSATokenRequest>(string_buf)?;
            let Token::Legacy(token) = context.tokens().get_user_sts_token().map_err(|err| {
                format!("no user is logged in (run `xodus-cli login` first): {err}")
            })?
            else {
                return Err("stored user STS token has an unsupported format".into());
            };
            let scope = if req.msa_full_trust {
                "service::user.auth.xboxlive.com::MBI_SSL"
            } else {
                "xboxlive.signin"
            };
            let device_token = context.device_token.as_ref().unwrap();
            let device_token_resp = xodus::api::live::exchange_device_token(
                &context.client,
                device_token.clone(),
                "{28C08266-F973-4AE6-FFE4-409B249F138F}".to_string(),
                "scope=service::user.auth.xboxlive.com::MBI_SSL".to_owned(),
                Some(soap::PolicyReference::token_broker()),
            )
            .await;

            let ms_device_rps_token = if let Some((Token::Compact(ms_device_token), Ok(lifetime))) =
                device_token_resp.ok().map(|t| {
                    let expiry = chrono::DateTime::parse_from_rfc3339(&t.lifetime.expires);
                    (t.into(), expiry)
                }) {
                Some((ms_device_token, lifetime.timestamp()))
            } else {
                None
            };

            let user_token = xodus::api::live::exchange_user_token(
                &context.client,
                token,
                "USERNAME".to_string(),
                device_token.clone(),
                None,
                Some("Silent".to_string()),
                req.client_id.clone(),
                &[
                    (
                        format!("scope={scope}&api-version=2.0&clientid={}", req.client_id),
                        Some(soap::PolicyReference::token_broker()),
                    ),
                    ("http://Passport.NET/tb".to_string(), None),
                ],
            )
            .await?;

            match user_token {
                ExchangeUserTokenOutcome::Issued(
                    soap::BodyContent::RequestSecurityTokenResponseCollection(mut collection),
                ) => {
                    if let Some(sts) = collection.security_tokens.pop() {
                        let address = sts.applies_to.endpoint_reference.address.clone();
                        let sts: Token = sts.into();
                        let address = if let Token::Legacy(legacy) = &sts {
                            legacy.key_name.clone().unwrap_or(address)
                        } else {
                            address
                        };
                        if let Err(err) = context.tokens().save_user_token(address, sts) {
                            log::warn!("Failed to persist refreshed STS token: {err}");
                        }
                    }
                    if collection.security_tokens.is_empty() {
                        return Err("token exchange returned too few security tokens".into());
                    }
                    let token = collection.security_tokens.remove(0);
                    let expiry = chrono::DateTime::parse_from_rfc3339(&token.lifetime.expires)?;
                    let token: Token = token.into();
                    let Token::Compact(user_token) = token else {
                        return Err("token exchange returned a non-compact user token".into());
                    };
                    let payload = MSATokenResponse {
                        token: user_token,
                        expiry: expiry.timestamp(),
                        device_expiry: ms_device_rps_token.as_ref().map(|(_, r)| *r).unwrap_or(0),
                        device_rps: ms_device_rps_token
                            .map(|(t, _)| t)
                            .unwrap_or_else(String::new),
                    };
                    let payload = quick_xml::se::to_string(&payload)?;
                    Ok(payload.as_bytes().to_vec())
                }
                ExchangeUserTokenOutcome::Fault(pp) => {
                    Err(format!("token exchange returned a fault: {pp:?}").into())
                }
                other => Err(format!("unexpected token exchange outcome: {other:?}").into()),
            }
        }
        XodusMessageType::UserIdentityRequest => {
            let Token::Legacy(user) = context.tokens().get_user_sts_token().map_err(|err| {
                format!("no user is logged in (run `xodus-cli login` first): {err}")
            })?
            else {
                return Err("stored user STS token has an unsupported format".into());
            };
            let device_token = context.device_token.as_ref().unwrap().clone();
            let xsts =
                xodus::api::xbox::run(&context.client, device_token, user, "http://xboxlive.com")
                    .await?;
            let payload = UserIdentityResponse {
                xuid: xsts.xuid().unwrap_or_default().to_string(),
                gamertag: xsts.gamertag().unwrap_or_default().to_string(),
                modern_gamertag: xsts.modern_gamertag().unwrap_or_default().to_string(),
                user_hash: xsts.user_hash().unwrap_or_default().to_string(),
                expiry: xsts.not_after.timestamp(),
            };
            Ok(quick_xml::se::to_string(&payload)?.into_bytes())
        }
        _ => Err("Unimplemented".into()),
    }
}
