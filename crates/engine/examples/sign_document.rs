use std::env;
use std::fs;

use oxide_engine::{ContentEngine, PdfSigner, Result, SignatureOptions};

fn main() -> Result<()> {
    let args = env::args().collect::<Vec<_>>();
    if args.len() != 5 {
        eprintln!(
            "usage: {} input.pdf private-key.pem signer-cert.pem output.pdf",
            args.first().map(String::as_str).unwrap_or("sign_document")
        );
        std::process::exit(2);
    }

    let input = fs::read(&args[1])?;
    let key_pem = fs::read_to_string(&args[2])?;
    let cert_pem = fs::read_to_string(&args[3])?;
    let engine = ContentEngine::open_bytes(input)?;
    let signer = PdfSigner::from_pem(&key_pem, &cert_pem, &[])?;
    let signed = engine.sign(
        &signer,
        &SignatureOptions {
            field_name: "OxideSignature1".to_string(),
            signer_name: Some("Oxide SDK Example Signer".to_string()),
            reason: Some("example signature".to_string()),
            location: Some("example".to_string()),
            signing_time: Some("D:20260622000000Z".to_string()),
            rect: Some([36.0, 36.0, 280.0, 96.0]),
            ..SignatureOptions::default()
        },
    )?;
    fs::write(&args[4], signed)?;
    Ok(())
}
