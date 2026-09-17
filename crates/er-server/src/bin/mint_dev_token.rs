use std::fs;

// src/bin/mint_dev_token.rs — two subcommands, plain std::env::args (no clap needed):
//   mint_dev_token jwks                          → prints the JWKS JSON for dev_public.pem
//   mint_dev_token token --sub test-doc          → prints a signed 24 h JWT
const KID: &str = "dev-key-1";

fn jwks() -> anyhow::Result<()> {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use rsa::{RsaPublicKey, pkcs8::DecodePublicKey, traits::PublicKeyParts};
    let key = RsaPublicKey::from_public_key_pem(&fs::read_to_string("dev/keys/dev_public.pem")?)?;
    let (n, e) = (
        URL_SAFE_NO_PAD.encode(key.n().to_bytes_be()),
        URL_SAFE_NO_PAD.encode(key.e().to_bytes_be()),
    );
    println!(
        "{}",
        serde_json::json!({ "keys": [{
        "kty": "RSA", "alg": "RS256", "use": "sig", "kid": KID, "n": n, "e": e }] })
    );
    Ok(())
}

fn token(sub: &str) -> anyhow::Result<()> {
    let header = jsonwebtoken::Header {
        alg: jsonwebtoken::Algorithm::RS256,
        kid: Some(KID.into()),
        ..Default::default()
    };
    let key = jsonwebtoken::EncodingKey::from_rsa_pem(&fs::read("dev/keys/dev_private.pem")?)?;
    let claims = serde_json::json!({
        "sub": sub,
        "iss": er_server::config::DEV_ISSUER,      // must match what S3 validates
        "token_use": "access",
        "exp": jsonwebtoken::get_current_timestamp() + 86_400,
    });
    println!("{}", jsonwebtoken::encode(&header, &claims, &key)?);
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("jwks") => jwks(),
        Some("token") => {
            // token --sub <subject>   (defaults to "test-doc")
            let sub = match args.get(2).map(String::as_str) {
                Some("--sub") => args.get(3).map(String::as_str).unwrap_or("test-doc"),
                None => "test-doc",
                Some(other) => anyhow::bail!("unexpected argument: {other}"),
            };
            token(sub)
        }
        _ => {
            anyhow::bail!("usage:\n  mint_dev_token jwks\n  mint_dev_token token --sub <subject>");
        }
    }
}
