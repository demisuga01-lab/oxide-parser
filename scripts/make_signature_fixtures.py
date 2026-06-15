#!/usr/bin/env python3
"""Generate signed-PDF fixtures for digital-signature verification tests.

Uses pyHanko + a self-signed RSA cert (generated here, so the test controls the
ground truth). Produces, under crates/engine/tests/fixtures/:

  sig_valid.pdf      - a single valid RSA/SHA-256 signature, covering whole file
  sig_two.pdf        - signed twice (two signature fields)

The tampered and modified-after-signing fixtures are derived from sig_valid.pdf
at TEST TIME (the Rust test mutates bytes / appends), so they don't need to be
committed and stay deterministic relative to sig_valid.pdf.

Run: py scripts/make_signature_fixtures.py
"""
import datetime
import io
import os

from cryptography import x509
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import rsa
from cryptography.x509.oid import NameOID

from cryptography.hazmat.primitives.serialization import pkcs12, BestAvailableEncryption

from pyhanko.sign import signers
from pyhanko.pdf_utils.incremental_writer import IncrementalPdfFileWriter
from pyhanko.sign.fields import SigFieldSpec, append_signature_field

FIX = os.path.join(os.path.dirname(__file__), "..", "crates", "engine", "tests", "fixtures")

# Base PDF to sign: reuse the committed, known-valid minimal.pdf fixture.
BASE_PDF = open(os.path.join(FIX, "minimal.pdf"), "rb").read()


def make_self_signed():
    key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
    name = x509.Name([
        x509.NameAttribute(NameOID.COMMON_NAME, "Oxide Test Signer"),
        x509.NameAttribute(NameOID.ORGANIZATION_NAME, "Oxide Test CA"),
        x509.NameAttribute(NameOID.COUNTRY_NAME, "US"),
    ])
    now = datetime.datetime(2024, 1, 1, tzinfo=datetime.timezone.utc)
    cert = (
        x509.CertificateBuilder()
        .subject_name(name)
        .issuer_name(name)
        .public_key(key.public_key())
        .serial_number(0x1234ABCD)
        .not_valid_before(now)
        .not_valid_after(now + datetime.timedelta(days=3650))
        .sign(key, hashes.SHA256())
    )
    return key, cert


def make_signer(key, cert):
    # pyHanko's SimpleSigner wants asn1crypto objects; bridge via a PKCS#12 file
    # built by `cryptography`, which pyHanko loads by path.
    p12 = pkcs12.serialize_key_and_certificates(
        name=b"oxide",
        key=key,
        cert=cert,
        cas=None,
        encryption_algorithm=BestAvailableEncryption(b"oxide"),
    )
    p12_path = os.path.join(FIX, "_signer.p12")
    with open(p12_path, "wb") as f:
        f.write(p12)
    signer = signers.SimpleSigner.load_pkcs12(p12_path, passphrase=b"oxide")
    os.remove(p12_path)
    return signer


def sign(in_bytes, signer, field_name):
    w = IncrementalPdfFileWriter(io.BytesIO(in_bytes))
    append_signature_field(w, SigFieldSpec(sig_field_name=field_name))
    meta = signers.PdfSignatureMetadata(field_name=field_name, md_algorithm="sha256")
    out = io.BytesIO()
    signers.sign_pdf(w, meta, signer=signer, output=out)
    return out.getvalue()


def main():
    os.makedirs(FIX, exist_ok=True)
    key, cert = make_self_signed()
    signer = make_signer(key, cert)

    # 1) Single valid signature.
    signed = sign(BASE_PDF, signer, "Sig1")
    with open(os.path.join(FIX, "sig_valid.pdf"), "wb") as f:
        f.write(signed)
    print("wrote sig_valid.pdf", len(signed), "bytes")

    # 2) Two signatures (second incremental signature over the first).
    signed2 = sign(signed, signer, "Sig2")
    with open(os.path.join(FIX, "sig_two.pdf"), "wb") as f:
        f.write(signed2)
    print("wrote sig_two.pdf", len(signed2), "bytes")

    print("cert CN: Oxide Test Signer  serial: 0x1234ABCD")


if __name__ == "__main__":
    main()
